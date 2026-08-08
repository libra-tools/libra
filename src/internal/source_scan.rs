//! Test-only Rust source scanning shared by the plan-20260714 inventory
//! guards (§C.4.1.1 mutable state, §C.4.3 GC object sources).
//!
//! Those guards ask a question no runtime check can answer: *is this
//! inventory complete over what the SOURCE actually does?* Answering it
//! means reading production code — and reading it TEXTUALLY is a trap that
//! bit the W1 registry four review rounds running. Splitting at the first
//! `#[cfg(test)]` truncated real code; matching balanced braces mistook a
//! comment, a struct field and a braced `use` for a test module; and
//! "mentions `test`" deleted `#[cfg(not(test))]`, which is production-only
//! code. Every one of those mistakes fails SILENTLY, in the direction of a
//! guard that passes while covering less than it claims.
//!
//! So the parsing lives here, once, on top of `syn`: parse the file, prune
//! the items that a production build genuinely cannot compile, and hand the
//! caller an AST to walk however its inventory needs.

use std::{fs, path::Path};

/// Three-valued evaluation of a `cfg` predicate with `test = false` and every
/// other predicate unknown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CfgValue {
    /// Provably off in a production build.
    Off,
    /// Provably on in a production build.
    On,
    /// Depends on a predicate this scan does not model (`unix`,
    /// `feature = "…"`, `debug_assertions`, …).
    Unknown,
}

/// Evaluate one `cfg` predicate as if `test` were NOT set.
///
/// Only [`CfgValue::Off`] means test-only. Merely MENTIONING `test` is not
/// enough: `not(test)` is production code (`utils/client_storage.rs`), and
/// `any(test, debug_assertions)` is compiled in any debug build
/// (`command/rename_detect.rs`).
pub fn evaluate_cfg(meta: &syn::Meta) -> CfgValue {
    match meta {
        syn::Meta::Path(path) if path.is_ident("test") => CfgValue::Off,
        syn::Meta::Path(_) | syn::Meta::NameValue(_) => CfgValue::Unknown,
        syn::Meta::List(list) => {
            let Ok(inner) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                // Unparseable predicate: unknown, i.e. KEEP the code.
                return CfgValue::Unknown;
            };
            if list.path.is_ident("not") {
                return match inner.first().map(evaluate_cfg) {
                    Some(CfgValue::Off) => CfgValue::On,
                    Some(CfgValue::On) => CfgValue::Off,
                    _ => CfgValue::Unknown,
                };
            }
            if list.path.is_ident("all") {
                let mut result = CfgValue::On;
                for predicate in &inner {
                    match evaluate_cfg(predicate) {
                        CfgValue::Off => return CfgValue::Off,
                        CfgValue::Unknown => result = CfgValue::Unknown,
                        CfgValue::On => {}
                    }
                }
                return result;
            }
            if list.path.is_ident("any") {
                let mut result = CfgValue::Off;
                for predicate in &inner {
                    match evaluate_cfg(predicate) {
                        CfgValue::On => return CfgValue::On,
                        CfgValue::Unknown => result = CfgValue::Unknown,
                        CfgValue::Off => {}
                    }
                }
                return result;
            }
            CfgValue::Unknown
        }
    }
}

/// Whether these attributes make the code they annotate impossible in a
/// production build. A parse failure means "not provably off" — KEEP.
pub fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| evaluate_cfg(&meta) == CfgValue::Off)
    })
}

/// Removes every `#[cfg(test)]`-gated item, field, variant, arm and statement
/// from an AST, so a consumer can walk what is left as production code.
struct TestItemStripper;

impl syn::visit_mut::VisitMut for TestItemStripper {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        file.items.retain(|item| !item_is_test_only(item));
        syn::visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut module.content {
            items.retain(|item| !item_is_test_only(item));
        }
        syn::visit_mut::visit_item_mod_mut(self, module);
    }

    fn visit_item_impl_mut(&mut self, block: &mut syn::ItemImpl) {
        block.items.retain(|item| {
            let attrs = match item {
                syn::ImplItem::Const(i) => Some(i.attrs.as_slice()),
                syn::ImplItem::Fn(i) => Some(i.attrs.as_slice()),
                syn::ImplItem::Type(i) => Some(i.attrs.as_slice()),
                syn::ImplItem::Macro(i) => Some(i.attrs.as_slice()),
                _ => None,
            };
            !attrs.is_some_and(is_test_only)
        });
        syn::visit_mut::visit_item_impl_mut(self, block);
    }

    fn visit_item_trait_mut(&mut self, item: &mut syn::ItemTrait) {
        item.items.retain(|item| {
            let attrs = match item {
                syn::TraitItem::Const(i) => Some(i.attrs.as_slice()),
                syn::TraitItem::Fn(i) => Some(i.attrs.as_slice()),
                syn::TraitItem::Type(i) => Some(i.attrs.as_slice()),
                syn::TraitItem::Macro(i) => Some(i.attrs.as_slice()),
                _ => None,
            };
            !attrs.is_some_and(is_test_only)
        });
        syn::visit_mut::visit_item_trait_mut(self, item);
    }

    fn visit_fields_mut(&mut self, fields: &mut syn::Fields) {
        match fields {
            syn::Fields::Named(named) => {
                let kept: Vec<syn::Field> = named
                    .named
                    .iter()
                    .filter(|field| !is_test_only(&field.attrs))
                    .cloned()
                    .collect();
                named.named = kept.into_iter().collect();
            }
            syn::Fields::Unnamed(unnamed) => {
                let kept: Vec<syn::Field> = unnamed
                    .unnamed
                    .iter()
                    .filter(|field| !is_test_only(&field.attrs))
                    .cloned()
                    .collect();
                unnamed.unnamed = kept.into_iter().collect();
            }
            syn::Fields::Unit => {}
        }
        syn::visit_mut::visit_fields_mut(self, fields);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        let kept: Vec<syn::Variant> = item
            .variants
            .iter()
            .filter(|variant| !is_test_only(&variant.attrs))
            .cloned()
            .collect();
        item.variants = kept.into_iter().collect();
        syn::visit_mut::visit_item_enum_mut(self, item);
    }

    fn visit_expr_match_mut(&mut self, expr: &mut syn::ExprMatch) {
        expr.arms.retain(|arm| !is_test_only(&arm.attrs));
        syn::visit_mut::visit_expr_match_mut(self, expr);
    }

    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.retain(|stmt| {
            let attrs = match stmt {
                syn::Stmt::Local(local) => Some(local.attrs.as_slice()),
                syn::Stmt::Macro(mac) => Some(mac.attrs.as_slice()),
                syn::Stmt::Expr(expr, _) => expr_attrs(expr),
                // Item statements are handled by the item pass below.
                syn::Stmt::Item(item) => return !item_is_test_only(item),
            };
            !attrs.is_some_and(is_test_only)
        });
        syn::visit_mut::visit_block_mut(self, block);
    }
}

/// The attributes of an item, for the variants that can carry code. Unknown
/// variants return `None` = "keep it": a false KEEP only risks an inventory
/// demanding a row for a fixture, while a false DROP hides real code.
fn item_attrs(item: &syn::Item) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => return None,
    })
}

fn item_is_test_only(item: &syn::Item) -> bool {
    item_attrs(item).is_some_and(is_test_only)
}

/// The attributes of an expression, for every variant that carries them.
/// `None` (an unmodelled variant) means KEEP, same rationale as
/// [`item_attrs`].
pub fn expr_attrs(expr: &syn::Expr) -> Option<&[syn::Attribute]> {
    Some(match expr {
        syn::Expr::Array(e) => &e.attrs,
        syn::Expr::Assign(e) => &e.attrs,
        syn::Expr::Async(e) => &e.attrs,
        syn::Expr::Await(e) => &e.attrs,
        syn::Expr::Binary(e) => &e.attrs,
        syn::Expr::Block(e) => &e.attrs,
        syn::Expr::Break(e) => &e.attrs,
        syn::Expr::Call(e) => &e.attrs,
        syn::Expr::Cast(e) => &e.attrs,
        syn::Expr::Closure(e) => &e.attrs,
        syn::Expr::Const(e) => &e.attrs,
        syn::Expr::Continue(e) => &e.attrs,
        syn::Expr::Field(e) => &e.attrs,
        syn::Expr::ForLoop(e) => &e.attrs,
        syn::Expr::Group(e) => &e.attrs,
        syn::Expr::If(e) => &e.attrs,
        syn::Expr::Index(e) => &e.attrs,
        syn::Expr::Infer(e) => &e.attrs,
        syn::Expr::Let(e) => &e.attrs,
        syn::Expr::Lit(e) => &e.attrs,
        syn::Expr::Loop(e) => &e.attrs,
        syn::Expr::Macro(e) => &e.attrs,
        syn::Expr::Match(e) => &e.attrs,
        syn::Expr::MethodCall(e) => &e.attrs,
        syn::Expr::Paren(e) => &e.attrs,
        syn::Expr::Path(e) => &e.attrs,
        syn::Expr::Range(e) => &e.attrs,
        syn::Expr::Reference(e) => &e.attrs,
        syn::Expr::Repeat(e) => &e.attrs,
        syn::Expr::Return(e) => &e.attrs,
        syn::Expr::Struct(e) => &e.attrs,
        syn::Expr::Try(e) => &e.attrs,
        syn::Expr::TryBlock(e) => &e.attrs,
        syn::Expr::Tuple(e) => &e.attrs,
        syn::Expr::Unary(e) => &e.attrs,
        syn::Expr::Unsafe(e) => &e.attrs,
        syn::Expr::While(e) => &e.attrs,
        syn::Expr::Yield(e) => &e.attrs,
        _ => return None,
    })
}

/// Parse one Rust source and prune its `#[cfg(test)]` code.
///
/// A source that does not parse is a HARD error: skipping it silently is how
/// a scan quietly stops covering a file.
pub fn parse_production_file(label: &str, source: &str) -> syn::File {
    let mut file = syn::parse_file(source)
        .unwrap_or_else(|error| panic!("`{label}` must parse as Rust: {error}"));
    syn::visit_mut::VisitMut::visit_file_mut(&mut TestItemStripper, &mut file);
    file
}

/// Every production Rust source under `src/`, parsed and pruned, as
/// `(repo-relative path, ast)`.
///
/// `extra_excludes` are repo-relative paths the caller has a documented
/// reason to skip. Whole test-only FILES (`tests.rs`, `tests/…`, `*_test.rs`)
/// carry no `#[cfg(test)]` marker of their own — their parent declares them
/// `#[cfg(test)] mod tests;` — so they are excluded by path.
pub fn production_files(extra_excludes: &[&str]) -> Vec<(String, syn::File)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut parsed = Vec::new();
    collect(&root, &root, extra_excludes, &mut parsed);
    parsed
}

fn collect(dir: &Path, root: &Path, excludes: &[&str], into: &mut Vec<(String, syn::File)>) {
    for entry in fs::read_dir(dir).expect("source dir must be readable") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, root, excludes, into);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative = format!("src/{}", relative.display());
        let is_test_file = relative.ends_with("/tests.rs")
            || relative.contains("/tests/")
            || relative.ends_with("_test.rs");
        if is_test_file || excludes.contains(&relative.as_str()) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("readable source");
        into.push((relative.clone(), parse_production_file(&relative, &source)));
    }
}

/// Every string literal passed to `.join("…")` on a receiver that names a
/// gitdir or the repository storage — i.e. the set of file names production
/// code can create under a repository's storage roots.
///
/// This is the forward half of the §C.4.3 file inventory: without it, a new
/// sidecar ships unclassified and the inventory silently stops describing
/// the repository.
pub fn storage_rooted_join_literals() -> std::collections::BTreeSet<String> {
    #[derive(Default)]
    struct Joins(std::collections::BTreeSet<String>);
    struct Idents(Vec<String>);

    impl<'ast> syn::visit::Visit<'ast> for Idents {
        fn visit_ident(&mut self, ident: &'ast syn::Ident) {
            // Underscores are stripped so `git_dir` — this codebase's usual
            // binding for `request_storage_path()` — matches the same rule
            // as `gitdir`. (A prior version missed every `git_dir` receiver,
            // which silently exempted the loose-object staging directory
            // from the inventory guard.)
            self.0
                .push(ident.to_string().to_lowercase().replace('_', ""));
        }
    }

    impl<'ast> syn::visit::Visit<'ast> for Joins {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "join"
                && let Some(syn::Expr::Lit(lit)) = call.args.first()
                && let syn::Lit::Str(name) = &lit.lit
            {
                // "Rooted at the repository" is decided by the RECEIVER's
                // identifiers: `gitdir`, `storage_path()`, `request_storage_path()`,
                // `scope.local_gitdir()?` all qualify. A join onto a workdir
                // path or an arbitrary temp dir does not.
                let mut idents = Idents(Vec::new());
                syn::visit::visit_expr(&mut idents, &call.receiver);
                let named_root = idents.0.iter().any(|ident| {
                    ident.contains("gitdir") || ident.contains("storage") || ident == "libradir"
                });
                // …or the receiver IS the storage root, spelled out:
                // `working_dir.join(".libra").join("automations.toml")` —
                // but NOT the user's global `~/.libra`: a home-rooted join
                // is a different filesystem tree, and sweeping it in makes
                // this guard assert repository claims about global files.
                let joined_root = match call.receiver.as_ref() {
                    syn::Expr::MethodCall(inner)
                        if inner.method == "join"
                            && matches!(
                                inner.args.first(),
                                Some(syn::Expr::Lit(lit))
                                    if matches!(&lit.lit, syn::Lit::Str(root) if root.value() == ".libra")
                            ) =>
                    {
                        let mut inner_idents = Idents(Vec::new());
                        syn::visit::visit_expr(&mut inner_idents, &inner.receiver);
                        !inner_idents.0.iter().any(|ident| ident.contains("home"))
                    }
                    _ => false,
                };
                if named_root || joined_root {
                    self.0.insert(name.value());
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    let mut found = Joins::default();
    for (_label, file) in production_files(&[]) {
        syn::visit::Visit::visit_file(&mut found, &file);
    }
    found.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stripper on the shapes that broke two textual predecessors.
    ///
    /// Each case corresponds to a REAL construct in this tree that a
    /// substring-based stripper mis-read as "a test item starts here", after
    /// which it deleted production code. Asserted in both directions:
    /// production code survives, genuine test-only code does not.
    #[test]
    fn cfg_test_stripping_keeps_exactly_the_production_half() {
        const FIXTURE: &str = r##"
            // A line comment that merely MENTIONS #[cfg(test)] — the text
            // stripper skipped to the next brace and ate this file.
            fn commented_marker() {
                let _ = "kept_after_comment";
            }

            struct Mixed {
                real: u8,
                #[cfg(test)]
                only_for_tests: u8,
            }

            #[cfg(test)]
            use std::collections::{BTreeMap, BTreeSet};

            fn after_braced_use() {
                let _ = "kept_after_use";
            }

            #[cfg(feature = "test-provider")]
            fn feature_gated_is_production() {
                let _ = "kept_feature_gate";
            }

            #[cfg(not(test))]
            fn production_only() {
                let _ = "kept_not_test";
            }

            #[cfg(any(test, debug_assertions))]
            fn test_or_debug() {
                let _ = "kept_any_test_or_debug";
            }

            #[cfg(all(feature = "x", test))]
            fn name_value_then_test() {
                let _ = "dropped_name_value_then_test";
            }

            fn attributed_statements() {
                #[cfg(test)]
                {
                    let _ = "dropped_stmt";
                }
                #[cfg(not(test))]
                {
                    let _ = "kept_stmt";
                }
                #[cfg(all(test, unix))]
                assert_eq!("dropped_macro_stmt", "");
                let _ = format!("kept_macro_stmt{}", "");
            }

            #[cfg(all(test, unix))]
            mod tests {
                fn fixture() {
                    let _ = "dropped_in_test_module";
                }
            }

            fn trailing_production() {
                let _ = "kept_after_test_module";
            }
        "##;

        #[derive(Default)]
        struct Literals(Vec<String>);
        impl<'ast> syn::visit::Visit<'ast> for Literals {
            fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
                self.0.push(lit.value());
            }
            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                if let Ok(args) = mac.parse_body_with(
                    syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
                ) {
                    let mut nested = Literals::default();
                    for arg in &args {
                        syn::visit::visit_expr(&mut nested, arg);
                    }
                    self.0.extend(nested.0);
                }
                syn::visit::visit_macro(self, mac);
            }
        }

        let file = parse_production_file("fixture.rs", FIXTURE);
        let mut found = Literals::default();
        syn::visit::Visit::visit_file(&mut found, &file);
        found.0.sort();
        found.0.retain(|value| !value.is_empty());
        assert_eq!(
            found.0,
            vec![
                "kept_after_comment",
                "kept_after_test_module",
                "kept_after_use",
                "kept_any_test_or_debug",
                "kept_feature_gate",
                "kept_macro_stmt{}",
                "kept_not_test",
                "kept_stmt",
            ],
            "production code must survive every marker-shaped construct, and \
             test-only code must not"
        );
    }
}
