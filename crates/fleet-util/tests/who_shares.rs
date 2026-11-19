//! Who shares an item chooses its crate (ADR-0008 §16).
//!
//! `fleet-util` holds what the Client and the operator tool share and the Server does not use;
//! `fleet-core` holds what the Server shares with either. This test reads the code under `src/` of
//! the Server, the Client and the tool that a crate root reaches through its modules, as a syntax
//! tree without the items under a `cfg` that cannot hold without `test`, so a module declared
//! under such a `cfg` is not read, and decides from the paths each names where every module of the
//! two crates belongs. Comments and literals are no tokens, so they name nothing. It also refuses
//! a `pub use` of a `fleet_util::` item and an alias of the crate, `self as` in a group included,
//! so an item has one path in use; a manifest of the Server that names `fleet-util`; and a manifest
//! of `fleet-util` that names a workspace crate but `fleet-core`. The tests of a crate are not
//! read: the tool's end-to-end tests run the Client.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::visit_mut::{self, VisitMut};
use syn::{
    Attribute, Expr, ForeignItem, ImplItem, Item, ItemUse, Meta, Stmt, Token, TraitItem, UseTree,
    Visibility,
};

/// The crates this test reads the sources of, in the order it reports them.
const CRATES: [&str; 5] = [
    "fleet-agent",
    "fleet-tools",
    "fleet-server",
    "fleet-core",
    "fleet-util",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Appends the shipped code of the module in `file`, and of every module it declares, whose files
/// are under `dir`, to `out`, a file once. A module declared under a `cfg` that cannot hold
/// without `test` is cut before it is reached, so its file is not read.
fn read_module(file: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, syn::File>) {
    if out.contains_key(file) {
        return;
    }
    let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    let code = shipped(&text).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
    let mut declared = Vec::new();
    module_files(&code.items, file, dir, &mut declared);
    out.insert(file.to_path_buf(), code);
    for (child, child_dir) in declared {
        read_module(&child, &child_dir, out);
    }
}

/// The file and the directory of submodules of each out-of-line module that `items` declare,
/// inline modules followed into their own items, whose modules are under `dir/<name>`.
fn module_files(items: &[Item], file: &Path, dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        // A `#[path]` module would sit where this reader does not look; none does today.
        assert!(
            !module.attrs.iter().any(|a| a.path().is_ident("path")),
            "{}: `#[path]` on `mod {}` is not followed",
            file.display(),
            module.ident
        );
        let name = module.ident.to_string();
        let name = name.trim_start_matches("r#");
        let child = dir.join(name);
        match &module.content {
            Some((_, inner)) => module_files(inner, file, &child, out),
            None => {
                let path = [dir.join(format!("{name}.rs")), child.join("mod.rs")]
                    .into_iter()
                    .find(|path| path.exists())
                    .unwrap_or_else(|| panic!("no file for `mod {name};` of {}", file.display()));
                out.push((path, child));
            }
        }
    }
}

// --- The shipped code of a file -----------------------------------------------------------------

/// The shipped code of a source text: its syntax tree, with every item, statement, field, variant
/// and arm under a `cfg` attribute that cannot hold without `test` cut. A unit test that names a
/// module uses nothing in the shipped code.
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

// --- The paths code names -----------------------------------------------------------------------

/// Every path under `root` that `tokens` name, as the segments after `root::`: a group is
/// expanded into a path for each of its members, `self` in a group ends the path, and `*` stands
/// as a segment. Tokens are read inside macro calls too, whose arguments the syntax tree keeps
/// unparsed.
fn paths_under(tokens: TokenStream, root: &str, out: &mut Vec<Vec<String>>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(group) => paths_under(group.stream(), root, out),
            TokenTree::Ident(ident) if ident == root && path_sep_at(&trees, i + 1) => {
                path_from(&trees[i + 3..], Vec::new(), out);
            }
            _ => {}
        }
    }
}

/// Whether `trees` hold `::` at `at`.
fn path_sep_at(trees: &[TokenTree], at: usize) -> bool {
    matches!(
        (trees.get(at), trees.get(at + 1)),
        (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
            if a.as_char() == ':' && a.spacing() == Spacing::Joint && b.as_char() == ':'
    )
}

/// The paths that `trees`, which follow `prefix::`, continue `prefix` into.
fn path_from(trees: &[TokenTree], mut prefix: Vec<String>, out: &mut Vec<Vec<String>>) {
    let mut at = 0;
    loop {
        match trees.get(at) {
            Some(TokenTree::Ident(ident)) if ident == "self" => break,
            Some(TokenTree::Ident(ident)) => prefix.push(ident.to_string()),
            Some(TokenTree::Punct(p)) if p.as_char() == '*' => {
                prefix.push("*".to_string());
                break;
            }
            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                let members: Vec<TokenTree> = group.stream().into_iter().collect();
                for member in
                    members.split(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
                {
                    if !member.is_empty() {
                        path_from(member, prefix.clone(), out);
                    }
                }
                return;
            }
            _ => break,
        }
        if !path_sep_at(trees, at + 1) {
            break;
        }
        at += 3;
    }
    out.push(prefix);
}

/// The paths under `root` that the shipped code of `file` names.
fn paths_named(file: &syn::File, root: &str) -> Vec<Vec<String>> {
    let mut paths = Vec::new();
    paths_under(file.to_token_stream(), root, &mut paths);
    paths
}

/// The modules of `crate_name` a file names: the first segment of each path under it.
fn modules_in(file: &syn::File, crate_name: &str) -> BTreeSet<String> {
    paths_named(file, crate_name)
        .into_iter()
        .filter_map(|path| path.into_iter().next())
        .filter(|first| first != "*")
        .collect()
}

/// Every `use` and `extern crate` item of a file, at every depth.
#[derive(Default)]
struct Imports<'a> {
    uses: Vec<&'a ItemUse>,
    extern_crates: Vec<&'a syn::ItemExternCrate>,
}

impl<'a> Visit<'a> for Imports<'a> {
    fn visit_item_use(&mut self, item: &'a ItemUse) {
        self.uses.push(item);
    }

    fn visit_item_extern_crate(&mut self, item: &'a syn::ItemExternCrate) {
        self.extern_crates.push(item);
    }
}

fn imports(file: &syn::File) -> Imports<'_> {
    let mut imports = Imports::default();
    imports.visit_file(file);
    imports
}

/// Whether `tree`, under the segments `prefix`, aliases the crate `fleet_util`: `fleet_util as …`,
/// or `self as …` in a `fleet_util::{…}` group.
fn aliases_the_crate(tree: &UseTree, prefix: &mut Vec<String>) -> bool {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            let found = aliases_the_crate(&path.tree, prefix);
            prefix.pop();
            found
        }
        UseTree::Rename(rename) => {
            (prefix.is_empty() && rename.ident == "fleet_util")
                || (prefix.as_slice() == ["fleet_util"] && rename.ident == "self")
        }
        UseTree::Group(group) => group.items.iter().any(|t| aliases_the_crate(t, prefix)),
        UseTree::Name(_) | UseTree::Glob(_) => false,
    }
}

/// What in a file aliases the crate `fleet_util`, so that a path could name its items without
/// `fleet_util::`.
fn aliases_in(file: &syn::File) -> Vec<String> {
    let imports = imports(file);
    let uses = imports
        .uses
        .iter()
        .filter(|item| aliases_the_crate(&item.tree, &mut Vec::new()))
        .map(|item| item.to_token_stream().to_string());
    let crates = imports
        .extern_crates
        .iter()
        .filter(|item| item.ident == "fleet_util" && item.rename.is_some())
        .map(|item| item.to_token_stream().to_string());
    uses.chain(crates).collect()
}

/// The `pub use` items of a file that re-export a `fleet_util::` item, or, with `every`, all of
/// them.
fn re_exports_in(file: &syn::File, every: bool) -> Vec<String> {
    imports(file)
        .uses
        .into_iter()
        .filter(|item| !matches!(item.vis, Visibility::Inherited))
        .filter(|item| {
            let mut paths = Vec::new();
            paths_under(item.to_token_stream(), "fleet_util", &mut paths);
            every || !paths.is_empty()
        })
        .map(|item| item.to_token_stream().to_string())
        .collect()
}

// --- The workspace ------------------------------------------------------------------------------

/// A workspace this test reads: the real one, or a fixture built like it.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn crate_dir(&self, krate: &str) -> PathBuf {
        self.root.join("crates").join(krate)
    }

    fn manifest(&self, krate: &str) -> toml::Table {
        let path = self.crate_dir(krate).join("Cargo.toml");
        fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .parse()
            .expect("the manifest parses")
    }

    /// The crate roots of `crates/<krate>/`: its library and its binaries, those its manifest
    /// names and those Cargo finds in `src/main.rs` and under `src/bin/`.
    fn roots(&self, krate: &str) -> BTreeSet<PathBuf> {
        let dir = self.crate_dir(krate);
        let src = dir.join("src");
        let manifest = self.manifest(krate);
        let mut roots = BTreeSet::new();
        let path_of = |target: &toml::Value| target.get("path")?.as_str().map(|p| dir.join(p));
        roots.extend(manifest.get("lib").and_then(path_of));
        let bins = manifest.get("bin").and_then(toml::Value::as_array);
        roots.extend(bins.into_iter().flatten().filter_map(path_of));
        roots.insert(src.join("lib.rs"));
        if manifest["package"]
            .get("autobins")
            .and_then(toml::Value::as_bool)
            != Some(false)
        {
            roots.insert(src.join("main.rs"));
            if let Ok(entries) = fs::read_dir(src.join("bin")) {
                for entry in entries {
                    let path = entry.expect("a directory entry").path();
                    roots.insert(if path.is_dir() {
                        path.join("main.rs")
                    } else {
                        path
                    });
                }
            }
        }
        roots.retain(|root| root.extension().is_some_and(|e| e == "rs") && root.exists());
        roots
    }

    /// The shipped code of every source file a crate root of `crates/<krate>/` reaches through
    /// its modules, with its path relative to the workspace root.
    fn sources(&self, krate: &str) -> Vec<(String, syn::File)> {
        let mut files = BTreeMap::new();
        for root in self.roots(krate) {
            let dir = root
                .parent()
                .expect("a crate root's directory")
                .to_path_buf();
            read_module(&root, &dir, &mut files);
        }
        files
            .into_iter()
            .map(|(file, code)| {
                // Named with `/` on every host, so a message reads alike on Windows.
                let name = file.strip_prefix(&self.root).unwrap_or(&file);
                let name = name
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                (name, code)
            })
            .collect()
    }

    /// The modules a crate root declares with `pub mod …;`, behind a `cfg` too.
    fn modules_of(&self, krate: &str) -> BTreeSet<String> {
        let root = self.crate_dir(krate).join("src/lib.rs");
        let text = fs::read_to_string(&root).expect("read a crate root");
        shipped(&text)
            .unwrap_or_else(|e| panic!("parse {}: {e}", root.display()))
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(m) if matches!(m.vis, Visibility::Public(_)) && m.content.is_none() => {
                    Some(m.ident.to_string())
                }
                _ => None,
            })
            .collect()
    }

    fn named_by(&self, krate: &str, crate_name: &str) -> BTreeSet<String> {
        self.sources(krate)
            .iter()
            .flat_map(|(_, code)| modules_in(code, crate_name))
            .collect()
    }

    /// The package names every dependency table of `crates/<krate>/Cargo.toml` names: normal,
    /// build and development dependencies, for every target.
    fn dependencies_of(&self, krate: &str) -> BTreeSet<String> {
        let manifest = self.manifest(krate);
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
    fn packages(&self) -> BTreeSet<String> {
        let read = |path: PathBuf| -> toml::Table {
            fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
                .parse()
                .expect("the manifest parses")
        };
        let root = read(self.root.join("Cargo.toml"));
        root["workspace"]["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|member| {
                let manifest = read(
                    self.root
                        .join(member.as_str().expect("a path"))
                        .join("Cargo.toml"),
                );
                manifest["package"]["name"]
                    .as_str()
                    .expect("a name")
                    .to_string()
            })
            .collect()
    }

    /// What in this workspace breaks clause 16, one line each.
    fn misplaced(&self) -> Vec<String> {
        let mut wrong = Vec::new();

        let client = self.named_by("fleet-agent", "fleet_util");
        let tool = self.named_by("fleet-tools", "fleet_util");
        let server = self.named_by("fleet-server", "fleet_util");
        for module in self.modules_of("fleet-util") {
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

        let server_core = self.named_by("fleet-server", "fleet_core");
        for module in self.modules_of("fleet-core") {
            if !server_core.contains(&module) {
                wrong.push(format!(
                    "fleet_core::{module}: no Server code names it, so it belongs in fleet-util"
                ));
            }
        }

        for krate in CRATES {
            for (file, code) in self.sources(krate) {
                for item in re_exports_in(&code, krate == "fleet-util") {
                    wrong.push(format!("{file}: re-exports: {item}"));
                }
                // An alias would let a `pub use` name the crate's items by another path.
                for alias in aliases_in(&code) {
                    wrong.push(format!("{file}: aliases fleet_util: {alias}"));
                }
            }
        }

        if self.dependencies_of("fleet-server").contains("fleet-util") {
            wrong.push("crates/fleet-server/Cargo.toml: names fleet-util".to_string());
        }
        let workspace = self.packages();
        for package in self.dependencies_of("fleet-util") {
            if workspace.contains(&package) && package != "fleet-core" {
                wrong.push(format!(
                    "crates/fleet-util/Cargo.toml: names {package}, a workspace crate but fleet-core"
                ));
            }
        }
        wrong
    }
}

/// Every module of `fleet-util` is named by the Client and by the tool and not by the Server;
/// every module of `fleet-core` is named by the Server; no crate re-exports a `fleet_util::`
/// item; the Server's manifest does not name `fleet-util`.
/// Verifies: ADR-0008
#[test]
fn each_shared_module_sits_in_the_crate_its_users_choose() {
    let wrong = Workspace {
        root: workspace_root(),
    }
    .misplaced();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// --- The per-item report ------------------------------------------------------------------------

/// The public items of the module whose shipped code is `items`, at `path`, declared in `dir`'s
/// files, with the modules declared there followed into their files.
fn public_items(items: &[Item], path: &[String], dir: &Path, out: &mut Vec<Vec<String>>) {
    for item in items {
        let (vis, ident) = match item {
            Item::Const(i) => (&i.vis, &i.ident),
            Item::Enum(i) => (&i.vis, &i.ident),
            Item::Fn(i) => (&i.vis, &i.sig.ident),
            Item::Static(i) => (&i.vis, &i.ident),
            Item::Struct(i) => (&i.vis, &i.ident),
            Item::Trait(i) => (&i.vis, &i.ident),
            Item::Type(i) => (&i.vis, &i.ident),
            Item::Union(i) => (&i.vis, &i.ident),
            Item::Mod(m) => {
                if !matches!(m.vis, Visibility::Public(_)) {
                    continue;
                }
                let mut inner = path.to_vec();
                inner.push(m.ident.to_string());
                let sub = dir.join(m.ident.to_string());
                if let Some((_, items)) = &m.content {
                    public_items(items, &inner, &sub, out);
                } else {
                    let file = [dir.join(format!("{}.rs", m.ident)), sub.join("mod.rs")]
                        .into_iter()
                        .find(|f| f.exists())
                        .unwrap_or_else(|| panic!("the file of module {}", inner.join("::")));
                    let text = fs::read_to_string(&file).expect("read a module");
                    let code =
                        shipped(&text).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
                    public_items(&code.items, &inner, &sub, out);
                }
                continue;
            }
            _ => continue,
        };
        if matches!(vis, Visibility::Public(_)) {
            let mut item_path = path.to_vec();
            item_path.push(ident.to_string());
            out.push(item_path);
        }
    }
}

/// What a use tree under `fleet_util::` binds a local name to: the name and the full path.
fn bindings(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            bindings(&path.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(name) if name.ident == "self" => {
            if let Some(last) = prefix.last() {
                out.push((last.clone(), prefix.clone()));
            }
        }
        UseTree::Name(name) => {
            let mut full = prefix.clone();
            full.push(name.ident.to_string());
            out.push((name.ident.to_string(), full));
        }
        UseTree::Rename(rename) => {
            let mut full = prefix.clone();
            if rename.ident != "self" {
                full.push(rename.ident.to_string());
            }
            out.push((rename.rename.to_string(), full));
        }
        UseTree::Group(group) => group.items.iter().for_each(|t| bindings(t, prefix, out)),
        UseTree::Glob(_) => {}
    }
}

/// The full `fleet_util::` paths a file names: directly, and through a name a `use` bound to a
/// path under the crate, such as `tree` after `use fleet_util::tree;`.
fn full_paths(file: &syn::File) -> Vec<Vec<String>> {
    let mut paths = paths_named(file, "fleet_util");
    let mut bound = Vec::new();
    for item in imports(file).uses {
        bindings(&item.tree, &mut Vec::new(), &mut bound);
    }
    let tokens = file.to_token_stream();
    for (local, full) in bound {
        let [root, ..] = full.as_slice() else {
            continue;
        };
        if root != "fleet_util" {
            continue;
        }
        let mut under = Vec::new();
        paths_under(tokens.clone(), &local, &mut under);
        paths.extend(
            under
                .into_iter()
                .map(|rest| [&full[1..], &rest[..]].concat()),
        );
    }
    paths
}

/// Lists each public item of `fleet-util` with the crates that name it in shipped code, as input
/// to a decision on whether the shared crates are cut again. A glob import names every item of
/// its module. It asserts nothing.
#[test]
#[ignore = "a report: run with --ignored --nocapture"]
fn report_the_crates_that_name_each_public_item_of_fleet_util() {
    let workspace = Workspace {
        root: workspace_root(),
    };
    let src = workspace.crate_dir("fleet-util").join("src");
    let lib = fs::read_to_string(src.join("lib.rs")).expect("read the crate root");
    let mut items = Vec::new();
    public_items(
        &shipped(&lib).expect("parse the crate root").items,
        &[],
        &src,
        &mut items,
    );

    let mut users: BTreeMap<Vec<String>, BTreeSet<&str>> = BTreeMap::new();
    for krate in ["fleet-agent", "fleet-tools", "fleet-server"] {
        let paths: Vec<Vec<String>> = workspace
            .sources(krate)
            .iter()
            .flat_map(|(_, code)| full_paths(code))
            .collect();
        for item in &items {
            let named = paths.iter().any(|path| {
                path.starts_with(item)
                    || path
                        .split_last()
                        .is_some_and(|(last, module)| last == "*" && item.starts_with(module))
            });
            if named {
                users.entry(item.clone()).or_default().insert(krate);
            }
        }
    }
    for item in &items {
        let crates = users.get(item).map_or_else(
            || "(none)".to_string(),
            |c| c.iter().copied().collect::<Vec<_>>().join(", "),
        );
        println!("fleet_util::{}: {crates}", item.join("::"));
    }
}

// --- The reader and the decision, on fixtures ---------------------------------------------------

fn modules_named(text: &str) -> Vec<String> {
    modules_in(&shipped(text).expect("the fixture parses"), "fleet_util")
        .into_iter()
        .collect()
}

fn aliases_of_the_crate(text: &str) -> Vec<String> {
    aliases_in(&shipped(text).expect("the fixture parses"))
}

/// The reader finds a module in a plain path and in a group, across lines, and ignores a longer
/// name that ends in the crate's.
#[test]
fn the_reader_finds_the_modules_code_names() {
    assert_eq!(
        modules_named("fn f() { let x = fleet_util::archive::detect(p); }"),
        ["archive"]
    );
    assert_eq!(
        modules_named("use fleet_util::{\n    nofollow::{read_privileged, Named},\n    tree,\n};"),
        ["nofollow", "tree"]
    );
    assert!(modules_named("use not_fleet_util::archive;").is_empty());
    assert!(modules_named("fn f() {} // fleet_util::tree").is_empty());
    assert_eq!(
        modules_named("fn f() { tracing::info!(\"{}\", fleet_util::tree::x()); }"),
        ["tree"]
    );
}

/// Comments, literals and test code name nothing; a lifetime and the code beside them stay.
#[test]
fn comments_literals_and_test_code_are_not_read() {
    let names = modules_named;
    assert!(names("/* fleet_util::tree /* nested */ fleet_util::archive */").is_empty());
    assert!(
        names(r#"fn f() { let m = "see fleet_util::tree \" fleet_util::archive"; }"#).is_empty()
    );
    assert!(names(r###"fn f() { let r = r#"a "quoted" fleet_util::tree"#; }"###).is_empty());
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
        names("fn f() { let c = cr\"\\\"; fleet_util::tree::x(); }"),
        ["tree"]
    );
    assert_eq!(
        names("#[cfg(not($m))]\nfn f() { fleet_util::tree::x(); }"),
        ["tree"]
    );
    assert_eq!(
        names("fn f() { let b = br\"\\\"; fleet_util::archive::x(); }"),
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
    // Test code inside an item, a statement and a field initialiser is cut as a whole item is.
    assert_eq!(
        names(concat!(
            "fn f() -> S {\n",
            "    #[cfg(test)]\n    fleet_util::tree::x();\n",
            "    #[cfg(test)]\n    *n += fleet_util::identity::x();\n",
            "    S { #[cfg(test)] t: fleet_util::password::x(), a: fleet_util::archive::x() }\n",
            "}\n",
            "impl S {\n    #[cfg(test)]\n    fn t() { fleet_util::nofollow::x(); }\n}",
        )),
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
    assert_eq!(
        aliases_of_the_crate("extern crate fleet_util as u;").len(),
        1
    );
    assert!(aliases_of_the_crate("use fleet_util::{self, tree};").is_empty());
    assert!(aliases_of_the_crate("use fleet_util::{nofollow::{self as nf}, tree};").is_empty());
}

/// A workspace laid out as this one, whose every file starts out as clause 16 wants it.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let fixture = Fixture {
            dir: tempfile::tempdir().expect("a temporary directory"),
        };
        let members = CRATES.map(|krate| format!("\"crates/{krate}\"")).join(", ");
        fixture.write(
            "Cargo.toml",
            &format!("[workspace]\nmembers = [{members}]\n"),
        );
        for krate in CRATES {
            let deps = match krate {
                "fleet-core" => "",
                "fleet-util" | "fleet-server" => "fleet-core = { path = \"../fleet-core\" }\n",
                _ => "fleet-core = { path = \"../fleet-core\" }\nfleet-util = { path = \"../fleet-util\" }\n",
            };
            fixture.write(
                &format!("crates/{krate}/Cargo.toml"),
                &format!("[package]\nname = \"{krate}\"\n\n[dependencies]\n{deps}"),
            );
        }
        fixture.write("crates/fleet-util/src/lib.rs", "pub mod tree;\n");
        fixture.write("crates/fleet-util/src/tree.rs", "pub fn is_kept() {}\n");
        fixture.write("crates/fleet-core/src/lib.rs", "pub mod version;\n");
        fixture.write("crates/fleet-core/src/version.rs", "pub fn current() {}\n");
        fixture.write("crates/fleet-agent/src/lib.rs", "use fleet_util::tree;\n");
        fixture.write(
            "crates/fleet-tools/src/main.rs",
            "fn main() { fleet_util::tree::is_kept(); }\n",
        );
        fixture.write(
            "crates/fleet-server/src/main.rs",
            "fn main() { fleet_core::version::current(); }\n",
        );
        fixture
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.dir.path().join(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("create a directory");
        fs::write(path, text).expect("write a fixture file");
    }

    fn misplaced(&self) -> Vec<String> {
        Workspace {
            root: self.dir.path().to_path_buf(),
        }
        .misplaced()
    }
}

/// The fixture as built breaks nothing, so each case below fails on its own change alone.
#[test]
fn a_workspace_as_clause_16_wants_it_passes() {
    assert_eq!(Fixture::new().misplaced(), Vec::<String>::new());
}

/// A module of `fleet-util` that the Client or the tool does not name fails, naming the module.
#[test]
fn a_util_module_one_of_its_users_does_not_name_fails() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-agent/src/lib.rs",
        "#[cfg(test)]\nuse fleet_util::tree;\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["fleet_util::tree: the Client does not name it"]
    );

    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-tools/src/main.rs",
        "fn main() {} // fleet_util::tree::is_kept();\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["fleet_util::tree: the operator tool does not name it"]
    );
}

/// A module of `fleet-util` that the Server names fails, naming the module.
#[test]
fn a_util_module_the_server_names_fails() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-server/src/main.rs",
        "fn main() { fleet_core::version::current(); fleet_util::tree::is_kept(); }\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["fleet_util::tree: the Server names it, so it belongs in fleet-core"]
    );
}

/// A module of `fleet-core` that no Server code names fails, naming the module.
#[test]
fn a_core_module_no_server_code_names_fails() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-server/src/main.rs",
        "fn main() {}\n#[cfg(test)]\nmod tests { use fleet_core::version; }\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["fleet_core::version: no Server code names it, so it belongs in fleet-util"]
    );
}

/// A `pub use` of a `fleet_util::` item fails, naming the re-export, and so does an alias of the
/// crate and any `pub use` inside `fleet-util`.
#[test]
fn a_re_export_of_a_util_item_or_an_alias_of_the_crate_fails() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-agent/src/lib.rs",
        "pub(crate) use fleet_util::tree;\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["crates/fleet-agent/src/lib.rs: re-exports: pub (crate) use fleet_util :: tree ;"]
    );

    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-agent/src/lib.rs",
        "use fleet_util::tree;\nuse fleet_util::{self as u};\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["crates/fleet-agent/src/lib.rs: aliases fleet_util: use fleet_util :: { self as u } ;"]
    );

    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-util/src/tree.rs",
        "pub use std::path::Path;\npub fn is_kept() {}\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["crates/fleet-util/src/tree.rs: re-exports: pub use std :: path :: Path ;"]
    );
}

/// A manifest of `fleet-server` that names `fleet-util` fails, naming the manifest, and so does
/// a manifest of `fleet-util` that names a workspace crate but `fleet-core`.
#[test]
fn a_manifest_that_names_the_wrong_crate_fails() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-server/Cargo.toml",
        "[package]\nname = \"fleet-server\"\n\n[dev-dependencies]\nu = { path = \"../fleet-util\", package = \"fleet-util\" }\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["crates/fleet-server/Cargo.toml: names fleet-util"]
    );

    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-util/Cargo.toml",
        "[package]\nname = \"fleet-util\"\n\n[target.'cfg(unix)'.dependencies]\nfleet-agent = { path = \"../fleet-agent\" }\n",
    );
    assert_eq!(
        fixture.misplaced(),
        ["crates/fleet-util/Cargo.toml: names fleet-agent, a workspace crate but fleet-core"]
    );
}

/// The file of a module declared under a test-only `cfg` is not read, so a Server fixture that
/// names a `fleet_util::` module, or re-exports one, counts for nothing; a module that a further
/// binary root under `src/bin/` declares is read.
#[test]
fn the_file_of_a_test_only_module_is_not_read() {
    let fixture = Fixture::new();
    fixture.write(
        "crates/fleet-server/src/main.rs",
        "fn main() { fleet_core::version::current(); }\nmod fleet;\n#[cfg(test)]\nmod test_support;\n",
    );
    fixture.write(
        "crates/fleet-server/src/fleet.rs",
        "#[cfg(all(test, unix))]\nmod fixtures;\n",
    );
    fixture.write(
        "crates/fleet-server/src/fleet/fixtures.rs",
        "pub use fleet_util::tree;\n",
    );
    fixture.write(
        "crates/fleet-server/src/test_support.rs",
        "pub fn f() { fleet_util::tree::is_kept(); }\n",
    );
    assert_eq!(fixture.misplaced(), Vec::<String>::new());

    let fixture = Fixture::new();
    fixture.write("crates/fleet-agent/src/lib.rs", "");
    fixture.write(
        "crates/fleet-agent/src/bin/stub.rs",
        "mod held;\nfn main() {}\n",
    );
    fixture.write(
        "crates/fleet-agent/src/bin/held.rs",
        "use fleet_util::tree;\n",
    );
    assert_eq!(fixture.misplaced(), Vec::<String>::new());
}
