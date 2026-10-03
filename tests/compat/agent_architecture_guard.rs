//! Architecture guard for the external-agent capture subsystem (AG-16/AG-24).
//!
//! Pins the boundary rules from `docs/development/tracing/agent.md`:
//! observed_agents (capture) stays decoupled from the internal AgentRuntime
//! and checkpoint layers, every known `AgentKind` resolves to a live
//! adapter, external agents cannot enter the static roster, and the SQL
//! CHECK constraint / doc roster / Rust enum stay in sync.

use std::{collections::BTreeSet, fs, path::Path};

use libra::internal::ai::observed_agents::{
    AgentKind, SlugLookup, agent_for, lookup_cli_slug, registration_for, registry,
};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Parse one repository-owned Rust source file.  The architecture checks below
/// intentionally work on syntax rather than comments or formatting so a
/// refactor cannot bypass a boundary merely by wrapping or reflowing a call.
fn parse_rust_source(relative_path: &str) -> (std::path::PathBuf, syn::File) {
    let path = repo_root().join(relative_path);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let file = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    (path, file)
}

/// Only an exact `#[cfg(test)]` subtree is test-only.  A compound predicate
/// such as `cfg(any(test, feature = "..."))` can compile in production and
/// therefore remains subject to the production boundary guards.
fn has_exact_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string().trim() == "test")
    })
}

fn syn_item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// Flatten imports so aliases and nested use groups cannot hide a forbidden
/// dependency from an architecture guard.
fn flatten_syn_use(tree: &syn::UseTree, prefix: &str, output: &mut Vec<String>) {
    let join = |prefix: &str, ident: &dyn std::fmt::Display| {
        if prefix.is_empty() {
            ident.to_string()
        } else {
            format!("{prefix}::{ident}")
        }
    };
    match tree {
        syn::UseTree::Path(path) => {
            flatten_syn_use(&path.tree, &join(prefix, &path.ident), output);
        }
        syn::UseTree::Name(name) if name.ident == "self" => output.push(prefix.to_string()),
        syn::UseTree::Rename(rename) if rename.ident == "self" => {
            output.push(prefix.to_string());
        }
        syn::UseTree::Name(name) => output.push(join(prefix, &name.ident)),
        syn::UseTree::Rename(rename) => output.push(join(prefix, &rename.ident)),
        syn::UseTree::Glob(_) => output.push(join(prefix, &"*")),
        syn::UseTree::Group(group) => {
            for item in &group.items {
                flatten_syn_use(item, prefix, output);
            }
        }
    }
}

fn syn_path_text(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Text between `start` and the first following `end` in one source file.
/// Both anchors are mandatory (ADR-ACF-10 guard migration): a moved, renamed
/// or deleted delimiter panics instead of silently widening the scanned
/// region to the rest of the file.
fn anchored_section<'a>(source: &'a str, file: &str, start: &str, end: &str) -> &'a str {
    let (_, after_start) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("{file}: start anchor `{start}` is missing"));
    let (section, _) = after_start
        .split_once(end)
        .unwrap_or_else(|| panic!("{file}: end delimiter `{end}` is missing after `{start}`"));
    section
}

fn top_level_function<'a>(file: &'a syn::File, name: &str) -> &'a syn::ItemFn {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Fn(function) if function.sig.ident == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected top-level function {name}"))
}

#[test]
fn private_capture_identity_has_no_source_opener() {
    use sha2::{Digest, Sha256};
    use syn::visit::Visit;

    #[derive(Default)]
    struct PrivateBoundary {
        paths: Vec<String>,
        sql: Vec<String>,
    }
    impl PrivateBoundary {
        /// Macro bodies are opaque to `syn::visit`; record their `a::b` token
        /// runs too so `ensure!(fs::read(..).is_ok(), ..)` cannot hide.
        fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            let mut run: Vec<String> = Vec::new();
            let mut colons = 0;
            for token in tokens {
                match token {
                    proc_macro2::TokenTree::Ident(ident) => {
                        if colons != 2 && !run.is_empty() {
                            self.paths.push(run.join("::"));
                            run.clear();
                        }
                        run.push(ident.to_string());
                        colons = 0;
                    }
                    proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ':' => {
                        colons += 1;
                    }
                    other => {
                        if !run.is_empty() {
                            self.paths.push(run.join("::"));
                            run.clear();
                        }
                        colons = 0;
                        if let proc_macro2::TokenTree::Group(group) = other {
                            self.scan_macro_tokens(group.stream());
                        }
                    }
                }
            }
            if !run.is_empty() {
                self.paths.push(run.join("::"));
            }
        }
    }
    impl<'ast> Visit<'ast> for PrivateBoundary {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if !has_exact_cfg_test(syn_item_attrs(item)) {
                syn::visit::visit_item(self, item);
            }
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            // Flatten imports so `use std::fs; fs::read(..)` or a renamed
            // `tokio::fs` import is checked by its real origin.
            flatten_syn_use(&item.tree, "", &mut self.paths);
            syn::visit::visit_item_use(self, item);
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.scan_macro_tokens(mac.tokens.clone());
            syn::visit::visit_macro(self, mac);
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.paths.push(syn_path_text(path));
            syn::visit::visit_path(self, path);
        }
        fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
            let value = value.value();
            if value.contains("SELECT") || value.contains("INSERT") || value.contains("UPDATE") {
                self.sql.push(value);
            }
        }
    }
    /// Any filesystem module segment (std, tokio or an imported `fs`), a file
    /// opener, or source-locator authority is outside the association port.
    fn opens_source(path: &str) -> bool {
        let segments = path.split("::").collect::<Vec<_>>();
        segments.contains(&"fs")
            || segments.contains(&"OpenOptions")
            || segments.windows(2).any(|pair| pair == ["File", "open"])
            || path.contains("TranscriptSource")
            || path.contains("authorized_read")
            || path.contains("hooks::")
    }
    fn source_openers(source: &str) -> Vec<String> {
        let file = syn::parse_file(source).expect("parse private boundary probe");
        let mut boundary = PrivateBoundary::default();
        boundary.visit_file(&file);
        boundary
            .paths
            .into_iter()
            .filter(|path| opens_source(path))
            .collect()
    }
    // Self-check the detector so a visitor regression cannot make the real
    // module scan below vacuously green.
    for probe in [
        "use std::fs; fn f() { let _ = fs::read(\"x\"); }",
        "use std::{fs as filesystem}; fn f() { let _ = filesystem::read(\"x\"); }",
        "use tokio::fs as tfs; async fn f() { let _ = tfs::read(\"x\").await; }",
        "fn f() { anyhow::ensure!(std::fs::metadata(\"x\").is_ok(), \"m\"); }",
        "fn f() { let _ = tokio::fs::File::open(\"x\"); }",
        "use std::fs::File; fn f() { let _ = File::open(\"x\"); }",
        "fn f() { let _ = OpenOptions::new().read(true).open(\"x\"); }",
        "fn f() { let _ = format!(\"{:?}\", File::open(\"x\")); }",
    ] {
        assert!(
            !source_openers(probe).is_empty(),
            "the private boundary guard must flag source openers in: {probe}"
        );
    }
    assert!(
        source_openers(
            "#[cfg(test)] mod tests { use std::fs; fn f() { let _ = fs::read(\"x\"); } }
             fn safe() { anyhow::ensure!(true, \"run `libra agent doctor`\"); }"
        )
        .is_empty(),
        "exact cfg(test) fixtures may create files; production stays guarded"
    );
    let (_, identity) = parse_rust_source("src/internal/ai/capture/pending_identity.rs");
    let mut boundary = PrivateBoundary::default();
    boundary.visit_file(&identity);
    for path in &boundary.paths {
        assert!(
            !opens_source(path),
            "private association must not acquire source authority: {path}"
        );
    }
    for sql in &boundary.sql {
        let identifiers = sql
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        assert!(
            !identifiers.iter().any(|id| id == "id" || id == "rowid"),
            "association cannot depend on SQLite row identities"
        );
    }
    assert!(
        boundary
            .sql
            .iter()
            .any(|sql| sql.contains("key = ? LIMIT 1"))
    );
    let (_, catalog) = parse_rust_source("src/internal/ai/capture/catalog.rs");
    let context = catalog
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == "PendingSessionContext" => Some(item),
            _ => None,
        })
        .expect("catalog-owned resolved context");
    assert!(
        context
            .fields
            .iter()
            .all(|field| matches!(field.vis, syn::Visibility::Inherited))
    );
    for attr in &context.attrs {
        if let syn::Meta::List(list) = &attr.meta {
            let tokens = list.tokens.to_string();
            assert!(
                !tokens.contains("Debug") && !tokens.contains("Serialize"),
                "resolved context must not expose sensitive identities"
            );
        }
    }
    let mut context_boundary = PrivateBoundary::default();
    context_boundary.visit_item_fn(top_level_function(
        &catalog,
        "resolve_pending_session_context",
    ));
    assert!(
        context_boundary
            .sql
            .iter()
            .any(|sql| sql.contains("WHERE session_id = ?") && sql.contains("LIMIT 1"))
    );

    // Export and all local byte helpers only consume committed checkpoint
    // tree data. Checking the complete module also covers helper refactors.
    let (_, checkpoint) = parse_rust_source("src/command/agent/checkpoint.rs");
    for name in [
        "export",
        "load_checkpoint_row",
        "load_checkpoint_transcript_bytes",
        "load_checkpoint_transcript_bytes_from_storage",
    ] {
        top_level_function(&checkpoint, name);
    }
    let mut export_boundary = PrivateBoundary::default();
    export_boundary.visit_file(&checkpoint);
    assert!(
        !export_boundary
            .paths
            .iter()
            .any(|path| path.contains("pending_identity")
                || path.contains("::pending::")
                || path.contains("MetadataKv")
                || path.contains("MetadataScope"))
    );
    assert!(
        !export_boundary
            .sql
            .iter()
            .any(|sql| sql.contains("metadata_kv"))
    );

    // ACF-15 may add only the exact cfg(test) canary module to this source.
    // Pin the pre-existing remainder: a serializer edit requires plan review,
    // not a quiet update of the canary or its expected snapshot. Re-pinned
    // for the plan-20260924 R85 cloud fix (legacy-row projection, single
    // failure emission), reviewed and recorded in that plan.
    let (_, cloud) = parse_rust_source("src/command/cloud/agent_capture.rs");
    assert!(matches!(&cloud.items[0], syn::Item::Use(item)
        if matches!(&item.tree, syn::UseTree::Path(path) if path.ident == "super")));
    assert!(matches!(&cloud.items[1], syn::Item::Mod(item)
        if item.ident == "tests" && has_exact_cfg_test(&item.attrs)));
    assert!(matches!(&cloud.items[2], syn::Item::Const(item)
        if item.ident == "AGENT_CAPTURE_LOCAL_PAGE_SIZE"));
    let source =
        fs::read_to_string(repo_root().join("src/command/cloud/agent_capture.rs")).unwrap();
    let (_, remainder) = source
        .split_once("\nconst AGENT_CAPTURE_LOCAL_PAGE_SIZE")
        .unwrap();
    let remainder = format!("const AGENT_CAPTURE_LOCAL_PAGE_SIZE{remainder}");
    assert_eq!(
        hex::encode(Sha256::digest(remainder.as_bytes())),
        "1d7babd3353b3f431d06accf3354b9f58bc7448645ebea95b03d224aa9040840",
        "ACF-15 cloud edit must be test-only; serializer change requires scope review"
    );
}

#[test]
fn capture_private_key_has_single_owner() {
    use syn::visit::Visit;

    const KEY_FILE_NAME: &str = "agent-capture-dedup-v1.key";
    const KEY_LOCATION_CONSTANTS: [&str; 2] =
        ["CAPTURE_DEDUP_SECRET_DIR", "CAPTURE_DEDUP_SECRET_FILE"];

    /// `test` and `all(.., test, ..)` can only compile under `cfg(test)`.
    /// Every other predicate, including `any(test, ..)` and `not(test)`, may
    /// compile in production and therefore stays guarded.
    fn cfg_requires_test(predicate: &syn::Meta) -> bool {
        match predicate {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|predicates| predicates.iter().any(cfg_requires_test)),
            _ => false,
        }
    }

    fn is_test_only(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|predicate| cfg_requires_test(&predicate))
        })
    }

    #[derive(Default)]
    struct KeyOwnerGuard {
        key_names: usize,
        loaders: usize,
        /// Non-test references to the key location constants. A second
        /// loader under another name would have to spell one of them (or the
        /// filename literal) to reach the key.
        location_constants: usize,
        hook_dependencies: Vec<String>,
    }

    impl KeyOwnerGuard {
        fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            for token in tokens {
                match token {
                    proc_macro2::TokenTree::Group(group) => self.scan_macro_tokens(group.stream()),
                    proc_macro2::TokenTree::Ident(ident) => {
                        if KEY_LOCATION_CONSTANTS.contains(&ident.to_string().as_str()) {
                            self.location_constants += 1;
                        }
                    }
                    proc_macro2::TokenTree::Literal(literal) => {
                        if literal.to_string() == format!("{KEY_FILE_NAME:?}") {
                            self.key_names += 1;
                        }
                    }
                    proc_macro2::TokenTree::Punct(_) => {}
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for KeyOwnerGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if !is_test_only(syn_item_attrs(item)) {
                syn::visit::visit_item(self, item);
            }
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            let attrs: &[syn::Attribute] = match item {
                syn::ImplItem::Const(item) => &item.attrs,
                syn::ImplItem::Fn(item) => &item.attrs,
                syn::ImplItem::Macro(item) => &item.attrs,
                syn::ImplItem::Type(item) => &item.attrs,
                _ => &[],
            };
            if !is_test_only(attrs) {
                syn::visit::visit_impl_item(self, item);
            }
        }

        fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
            if value.value() == KEY_FILE_NAME {
                self.key_names += 1;
            }
        }

        fn visit_ident(&mut self, ident: &'ast syn::Ident) {
            if KEY_LOCATION_CONSTANTS.contains(&ident.to_string().as_str()) {
                self.location_constants += 1;
            }
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.scan_macro_tokens(mac.tokens.clone());
            syn::visit::visit_macro(self, mac);
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if item
                .sig
                .ident
                .to_string()
                .starts_with("load_capture_dedup_secret")
            {
                self.loaders += 1;
            }
            syn::visit::visit_item_fn(self, item);
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if item
                .sig
                .ident
                .to_string()
                .starts_with("load_capture_dedup_secret")
            {
                self.loaders += 1;
            }
            syn::visit::visit_impl_item_fn(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            self.hook_dependencies
                .extend(paths.into_iter().filter(|path| {
                    path.contains("hooks::runtime") || path.contains("hooks::providers")
                }));
            syn::visit::visit_item_use(self, item);
        }
    }

    fn scan(source: &str) -> KeyOwnerGuard {
        let file = syn::parse_file(source).expect("parse key-owner guard source");
        let mut guard = KeyOwnerGuard::default();
        guard.visit_file(&file);
        guard
    }

    // Self-check the detector so a visitor regression cannot make the
    // repository walk below vacuously green.
    assert_eq!(
        scan(
            "use crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_FILE;
             fn open(storage: &Path) -> PathBuf { storage.join(key::CAPTURE_DEDUP_SECRET_DIR) }
             fn label() -> String { format!(\"{}\", key::CAPTURE_DEDUP_SECRET_FILE) }
             impl Doctor { fn read_capture_key(&self) { let _ = \"agent-capture-dedup-v1.key\"; } }
             #[cfg(any(test, feature = \"x\"))]
             use crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_DIR as Dir;
             #[cfg(not(test))] fn production() { let _ = key::CAPTURE_DEDUP_SECRET_FILE; }"
        )
        .location_constants,
        5,
        "the guard must flag production key-location references, including in macros, \
         aliases and non-test-only cfg predicates"
    );
    let test_only = scan(
        "#[cfg(all(test, unix))]
         use crate::internal::ai::capture::key::{CAPTURE_DEDUP_SECRET_DIR, CAPTURE_DEDUP_SECRET_FILE};
         #[cfg(test)]
         mod tests { fn path() { let _ = (key::CAPTURE_DEDUP_SECRET_DIR, \"agent-capture-dedup-v1.key\"); } }
         impl Fixture { #[cfg(all(unix, test))] fn key(&self) { let _ = key::CAPTURE_DEDUP_SECRET_FILE; } }",
    );
    assert_eq!(
        (test_only.location_constants, test_only.key_names),
        (0, 0),
        "test-only references must stay outside the production key-owner guard"
    );
    let impostor = scan(
        "impl Doctor { fn load_capture_dedup_secret_for_doctor(&self) {} }
         fn mint() -> String { format!(\"{}\", \"agent-capture-dedup-v1.key\") }",
    );
    assert_eq!(
        (impostor.loaders, impostor.key_names),
        (1, 1),
        "the guard must flag loader methods and filename literals inside macros"
    );

    let (_, key) = parse_rust_source("src/internal/ai/capture/key.rs");
    let mut owner = KeyOwnerGuard::default();
    owner.visit_file(&key);
    assert_eq!(
        owner.key_names, 1,
        "the shared owner must keep the original key filename"
    );
    assert!(
        owner.loaders > 0,
        "the shared owner must own initialization"
    );
    assert!(
        owner.hook_dependencies.is_empty(),
        "the shared owner must not depend on hooks: {:?}",
        owner.hook_dependencies
    );

    // Walk every production source, not only the AI subtree: a doctor,
    // import, or cloud command must not grow a second key reader either.
    let mut paths = Vec::new();
    let mut directories = vec![repo_root().join("src")];
    let owner_path = repo_root().join("src/internal/ai/capture/key.rs");
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).expect("read capture owner directory") {
            let entry = entry.expect("read capture owner entry");
            let path = entry.path();
            let kind = entry.file_type().expect("classify capture owner entry");
            if kind.is_dir() {
                directories.push(path);
            } else if kind.is_file()
                && path.extension().is_some_and(|extension| extension == "rs")
                && path != owner_path
            {
                paths.push(path);
            }
        }
    }
    for expected in [
        "src/command/agent/import.rs",
        "src/internal/ai/hooks/runtime.rs",
        "src/main.rs",
    ] {
        assert!(
            paths.contains(&repo_root().join(expected)),
            "the key-owner walk must cover {expected}"
        );
    }
    for path in paths {
        let source = fs::read_to_string(&path).expect("read capture owner source");
        let other = scan(&source);
        assert_eq!(
            other.key_names,
            0,
            "{} must not declare a second key location",
            path.display()
        );
        assert_eq!(
            other.loaders,
            0,
            "{} must delegate rather than duplicate the loader",
            path.display()
        );
        assert_eq!(
            other.location_constants,
            0,
            "{} must not reference the private key location outside test code; \
             delegate to src/internal/ai/capture/key.rs",
            path.display()
        );
    }
}

fn top_level_struct<'a>(file: &'a syn::File, name: &str) -> &'a syn::ItemStruct {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(structure) if structure.ident == name => Some(structure),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected top-level struct {name}"))
}

fn source_words(value: &str) -> Vec<String> {
    value
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

fn is_capture_catalog_table(word: &str) -> bool {
    matches!(word, "AGENT_SESSION" | "AGENT_CHECKPOINT")
}

fn sql_capture_catalog_mutation(sql: &str) -> Option<String> {
    sql_table_mutation(sql, is_capture_catalog_table)
}

/// ACF-08: coverage-claim reservation, renewal, binding and abandonment are
/// coverage-gate policy. An entrypoint that spells this DML itself keeps a
/// second copy of the fence/lease semantics outside `coverage_gate`.
fn sql_coverage_claim_mutation(sql: &str) -> Option<String> {
    sql_table_mutation(sql, |word| word == "AGENT_COVERAGE_CLAIM")
}

/// Name the table a DML/DDL statement mutates when `is_table` selects it.
fn sql_table_mutation(sql: &str, is_table: impl Fn(&str) -> bool) -> Option<String> {
    let words = source_words(sql);
    let skip_conflict_mode = |word: &str| {
        matches!(
            word,
            "OR" | "REPLACE" | "IGNORE" | "ABORT" | "FAIL" | "ROLLBACK"
        )
    };
    for (index, word) in words.iter().enumerate() {
        let mut next = index + 1;
        match word.as_str() {
            "INSERT" | "REPLACE" | "MERGE" => {
                while words.get(next).is_some_and(|word| skip_conflict_mode(word)) {
                    next += 1;
                }
                if words.get(next).is_some_and(|word| word == "INTO") {
                    next += 1;
                }
            }
            "UPDATE" => {
                while words.get(next).is_some_and(|word| skip_conflict_mode(word)) {
                    next += 1;
                }
            }
            "DELETE" => {
                if words.get(next).is_some_and(|word| word == "FROM") {
                    next += 1;
                }
            }
            "ALTER" | "DROP" => {
                if words.get(next).is_some_and(|word| word == "TABLE") {
                    next += 1;
                }
            }
            _ => continue,
        }
        if let Some(table) = words.get(next).filter(|word| is_table(word)) {
            return Some(table.to_ascii_lowercase());
        }
    }
    None
}

fn task_section<'a>(document: &'a str, task: &str) -> &'a str {
    let heading = format!("### Task {task}");
    let rest = document
        .split_once(&heading)
        .unwrap_or_else(|| panic!("missing {heading} in plan document"))
        .1;
    rest.split("\n### Task ").next().unwrap_or(rest)
}

/// Capture modules must not import the internal AgentRuntime or the
/// checkpoint-writer layers. Allowed seams: `hooks::{lifecycle,provider}`
/// (hook contracts), `completion` (shared usage model), `session` (session
/// context types), and `tool_call_record` (plan-20260920 RC-04). The
/// former `orchestrator::types` exception for `ToolCallRecord` is closed.
/// Anything else from the runtime side is a boundary violation.
///
/// The check is AST-based (`syn`): use-trees are flattened (so grouped and
/// nested-grouped imports cannot slip through), inline fully-qualified
/// paths are visited, and items annotated `#[cfg(test)]` are pruned —
/// schema-lockstep tests may deliberately drive runtime writers (e.g.
/// derived.rs's normalized-event integration test).
#[test]
fn observed_agent_modules_do_not_import_runtime_or_checkpoint_layers() {
    use syn::visit::Visit;

    /// Why a resolved path is out of bounds, or `None` when it is fine.
    ///
    /// `original` is a `::`-joined path as written. Leading `crate::` /
    /// `super::` chains are normalized away; the remainder is judged
    /// ai-relative when it came through `internal::ai::` explicitly or
    /// through enough `super::` hops to escape the capture module
    /// (`ai_root_supers` = 2 for files directly under `observed_agents/`,
    /// 3 for `builtin/`, …). Bare paths (`runtime::Handle` from a `use
    /// tokio::runtime` import) are not judged — their `use` item is.
    /// Module-root imports/renames of `crate::internal` / the ai root and
    /// root-level globs are forbidden outright: they would let an alias
    /// (`use crate::internal::ai as x; x::runtime::…`) evade the check.
    fn forbidden_reason(original: &str, ai_root_supers: usize) -> Option<String> {
        let had_crate = original.starts_with("crate::");
        let mut path = original.strip_prefix("crate::").unwrap_or(original);
        let mut supers = 0usize;
        while let Some(rest) = path.strip_prefix("super::") {
            path = rest;
            supers += 1;
        }
        if had_crate && (path == "internal" || path == "internal::ai") {
            return Some(
                "module-root import/rename of crate::internal(::ai) — alias bypass".to_string(),
            );
        }
        let candidate = if let Some(rest) = path
            .strip_prefix("internal::ai::")
            .or_else(|| path.strip_prefix("ai::"))
        {
            rest
        } else if !had_crate && supers >= ai_root_supers {
            path
        } else {
            return None;
        };
        if candidate.is_empty() {
            return Some("aliasing the internal::ai root — alias bypass".to_string());
        }
        if candidate == "*" {
            return Some("glob import from the internal::ai root".to_string());
        }
        if candidate == "hooks" || candidate == "hooks::*" {
            return Some(
                "module-root/glob import of internal::ai::hooks (surfaces hooks::runtime)"
                    .to_string(),
            );
        }
        for module in ["agent", "runtime", "agent_run", "history"] {
            if candidate == module || candidate.starts_with(&format!("{module}::")) {
                return Some(format!("internal::ai::{module}"));
            }
        }
        if candidate == "hooks::runtime" || candidate.starts_with("hooks::runtime::") {
            return Some("internal::ai::hooks::runtime".to_string());
        }
        if candidate == "orchestrator" || candidate.starts_with("orchestrator::") {
            return Some("internal::ai::orchestrator".to_string());
        }
        None
    }

    /// Flatten a use-tree into fully-qualified `::`-joined paths.
    fn flatten_use(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        let join = |prefix: &str, ident: &dyn std::fmt::Display| {
            if prefix.is_empty() {
                ident.to_string()
            } else {
                format!("{prefix}::{ident}")
            }
        };
        match tree {
            syn::UseTree::Path(path) => {
                flatten_use(&path.tree, &join(prefix, &path.ident), out);
            }
            // `{self}` / `{self as x}` denote the prefix module itself —
            // normalize so root-alias checks fire on the real path.
            syn::UseTree::Name(name) if name.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Rename(rename) if rename.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Name(name) => out.push(join(prefix, &name.ident)),
            syn::UseTree::Rename(rename) => out.push(join(prefix, &rename.ident)),
            syn::UseTree::Glob(_) => out.push(join(prefix, &"*")),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    flatten_use(item, prefix, out);
                }
            }
        }
    }

    /// Only the exact `#[cfg(test)]` predicate prunes — `cfg(not(test))`
    /// (and any compound predicate) is production code and stays guarded.
    fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string().trim() == "test")
        })
    }

    fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
        match item {
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
            _ => &[],
        }
    }

    struct BoundaryGuard {
        violations: Vec<String>,
        /// `super::` hops from this file's module to the `internal::ai`
        /// root: 2 for files directly under `observed_agents/`, 3 for
        /// `builtin/`, … Used to judge super-relative paths correctly.
        ai_root_supers: usize,
    }

    impl<'ast> Visit<'ast> for BoundaryGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Prune #[cfg(test)] subtrees — test-only seams are allowed.
            if has_cfg_test(item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = forbidden_reason(&path, self.ai_root_supers) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
            }
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let joined = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(reason) = forbidden_reason(&joined, self.ai_root_supers) {
                self.violations.push(format!("path {joined} → {reason}"));
            }
            syn::visit::visit_path(self, path);
        }
    }

    let dir = repo_root().join("src/internal/ai/observed_agents");
    let mut checked = 0usize;
    let mut stack = vec![dir];
    let mut violations = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("read observed_agents dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read source file");
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            checked += 1;
            let relative = path
                .strip_prefix(repo_root().join("src/internal/ai/observed_agents"))
                .expect("scanned file lives under observed_agents");
            let depth = relative.components().count().saturating_sub(1);
            // `mod.rs` IS its directory's module — one super fewer than a
            // leaf file at the same directory level.
            let is_mod_rs = relative.file_name().is_some_and(|name| name == "mod.rs");
            let mut guard = BoundaryGuard {
                violations: Vec::new(),
                ai_root_supers: 2 + depth - usize::from(is_mod_rs),
            };
            guard.visit_file(&file);
            for violation in guard.violations {
                violations.push(format!("{}: {violation}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "observed_agents must stay decoupled from the internal AgentRuntime/checkpoint \
         layers:\n{}",
        violations.join("\n")
    );
    assert!(
        checked >= 8,
        "expected to scan the observed_agents sources, got {checked}"
    );
}

/// ADR-ACF-10 (ACF-19): the capture layer sits below the hook entry. No
/// production capture module may reach back into the hook runtime, the legacy
/// intent writer, or the provider adapters (`hooks::{runtime, intent,
/// providers}`); the hook contracts (`hooks::{lifecycle, provider}`) stay
/// allowed. Use trees are flattened and every inline path is visited, so a
/// grouped import or a fully-qualified call cannot slip through, and a
/// module-root import or alias of `hooks` is rejected because it would let a
/// later `alias::runtime::…` path evade the segment check. Exact
/// `#[cfg(test)]` items and the out-of-line `*_tests.rs` modules are exempt.
#[test]
fn capture_modules_do_not_import_hook_entry_layers() {
    use syn::visit::Visit;

    const HOOK_ENTRY_LAYERS: [&str; 3] = ["runtime", "intent", "providers"];

    /// `hooks::{runtime,intent,providers}` segments, plus a name that
    /// `hooks/mod.rs` re-exports from one of those layers (for example
    /// `hooks::claude_provider` or `hooks::HookTarget`).
    fn path_violation(segments: &[String], reexported: &BTreeSet<String>) -> Option<String> {
        segments.windows(2).find_map(|pair| {
            if pair[0] != "hooks" {
                None
            } else if HOOK_ENTRY_LAYERS.contains(&pair[1].as_str()) {
                Some(format!("hooks::{}", pair[1]))
            } else if reexported.contains(&pair[1]) {
                Some(format!("hooks::{} (re-exported entry layer)", pair[1]))
            } else {
                None
            }
        })
    }

    fn import_violation(path: &str, reexported: &BTreeSet<String>) -> Option<String> {
        let segments: Vec<String> = path
            .split(" as ")
            .next()
            .unwrap_or(path)
            .split("::")
            .map(str::to_string)
            .collect();
        if let Some(layer) = path_violation(&segments, reexported) {
            return Some(layer);
        }
        // `use …::hooks;`, `use …::hooks as h;`, `use …::hooks::{self}` and
        // `use …::hooks::*` all surface the entry layers under another name.
        match segments.as_slice() {
            [.., last] if last == "hooks" => Some("module-root import of hooks".to_string()),
            [.., hooks, glob] if hooks == "hooks" && glob == "*" => {
                Some("glob import of hooks".to_string())
            }
            _ => None,
        }
    }

    fn flatten_use_with_alias(tree: &syn::UseTree, prefix: &str, output: &mut Vec<String>) {
        let join = |prefix: &str, ident: &dyn std::fmt::Display| {
            if prefix.is_empty() {
                ident.to_string()
            } else {
                format!("{prefix}::{ident}")
            }
        };
        match tree {
            syn::UseTree::Path(path) => {
                flatten_use_with_alias(&path.tree, &join(prefix, &path.ident), output);
            }
            syn::UseTree::Name(name) if name.ident == "self" => output.push(prefix.to_string()),
            syn::UseTree::Rename(rename) if rename.ident == "self" => {
                output.push(format!("{prefix} as {}", rename.rename));
            }
            syn::UseTree::Name(name) => output.push(join(prefix, &name.ident)),
            syn::UseTree::Rename(rename) => {
                output.push(format!(
                    "{} as {}",
                    join(prefix, &rename.ident),
                    rename.rename
                ));
            }
            syn::UseTree::Glob(_) => output.push(join(prefix, &"*")),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    flatten_use_with_alias(item, prefix, output);
                }
            }
        }
    }

    struct HookEntryImportGuard<'a> {
        reexported: &'a BTreeSet<String>,
        violations: Vec<String>,
    }

    impl<'ast> Visit<'ast> for HookEntryImportGuard<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_use_with_alias(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = import_violation(&path, self.reexported) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let segments: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if let Some(reason) = path_violation(&segments, self.reexported) {
                self.violations
                    .push(format!("path {} → {reason}", segments.join("::")));
            }
            syn::visit::visit_path(self, path);
        }
    }

    fn violations(file: &syn::File, reexported: &BTreeSet<String>) -> Vec<String> {
        let mut guard = HookEntryImportGuard {
            reexported,
            violations: Vec::new(),
        };
        guard.visit_file(file);
        guard.violations
    }

    // Names `hooks/mod.rs` re-exports from the entry layers.
    let (hooks_mod_path, hooks_mod) = parse_rust_source("src/internal/ai/hooks/mod.rs");
    let mut reexported = BTreeSet::new();
    for item in &hooks_mod.items {
        if let syn::Item::Use(item) = item
            && matches!(item.vis, syn::Visibility::Public(_))
        {
            let mut paths = Vec::new();
            flatten_use_with_alias(&item.tree, "", &mut paths);
            for path in paths {
                let first = path.split("::").next().unwrap_or_default();
                if HOOK_ENTRY_LAYERS.contains(&first) {
                    let exported = match path.split_once(" as ") {
                        Some((_, alias)) => alias.to_string(),
                        None => path.rsplit("::").next().unwrap_or_default().to_string(),
                    };
                    reexported.insert(exported);
                }
            }
        }
    }
    for expected in [
        "claude_provider",
        "HookTarget",
        "process_hook_event_with_target",
    ] {
        assert!(
            reexported.contains(expected),
            "{}: expected the entry-layer re-export {expected}: {reexported:?}",
            hooks_mod_path.display()
        );
    }

    // Non-vacuity: each banned spelling is reported once; the hook contracts
    // and exact `#[cfg(test)]` items stay allowed.
    let fixture = syn::parse_file(
        r#"
        use crate::internal::ai::hooks::runtime::HookTarget;
        use crate::internal::ai::hooks::{intent::AI_SESSION_TYPE, provider::HookProvider};
        use crate::internal::ai::hooks as entry;
        use super::super::hooks::*;
        use crate::internal::ai::hooks::lifecycle::LifecycleEventKind;
        use crate::internal::ai::hooks::LifecycleEventKind as Kind;
        fn regressed() {
            crate::internal::ai::hooks::providers::claude_provider();
            crate::internal::ai::hooks::claude_provider();
        }
        #[cfg(test)]
        mod tests {
            use crate::internal::ai::hooks::providers::claude_provider;
        }
        "#,
    )
    .expect("parse hook-entry import guard fixture");
    let fixture_violations = violations(&fixture, &reexported);
    assert_eq!(
        fixture_violations.len(),
        6,
        "hook-entry import guard self-test: {fixture_violations:#?}"
    );
    for expected in [
        "hooks::runtime",
        "hooks::intent",
        "module-root import of hooks",
        "glob import of hooks",
        "hooks::providers",
        "hooks::claude_provider (re-exported entry layer)",
    ] {
        assert!(
            fixture_violations
                .iter()
                .any(|violation| violation.ends_with(expected)),
            "hook-entry import guard must report {expected}: {fixture_violations:#?}"
        );
    }

    let capture_root = repo_root().join("src/internal/ai/capture");
    let mut stack = vec![capture_root.clone()];
    let mut scanned = BTreeSet::new();
    let mut found = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        {
            let path = entry.expect("read capture module entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let relative = path
                .strip_prefix(&capture_root)
                .expect("scanned file lives under capture")
                .display()
                .to_string();
            for violation in violations(&file, &reexported) {
                found.push(format!("{relative}: {violation}"));
            }
            scanned.insert(relative);
        }
    }
    for expected in [
        "live.rs",
        "live_pipeline.rs",
        "live_checkpoint.rs",
        "scope_binding.rs",
        "test_support.rs",
        "coordinator.rs",
    ] {
        assert!(
            scanned.contains(expected),
            "the capture import scan must cover {expected}: {scanned:?}"
        );
    }
    assert!(
        !scanned.contains("live_pipeline_tests.rs"),
        "out-of-line test modules are exempt from the production import scan"
    );
    assert!(
        found.is_empty(),
        "capture modules must not import the hook entry layers (hooks::runtime/intent/providers):\n{}",
        found.join("\n")
    );
}

/// ACF-01 keeps parsing at the untrusted edge: ingress may depend on the
/// canonical hook contracts, but it must not acquire a database, catalog,
/// checkpoint/ref, filesystem-store, or persistence handle. The live hook
/// adapter must likewise delegate decoding to that boundary instead of
/// reintroducing a second parser in `runtime.rs`.
#[test]
fn capture_ingress_has_no_store_dependency() {
    use syn::visit::Visit;

    fn flatten_use(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        let join = |prefix: &str, ident: &dyn std::fmt::Display| {
            if prefix.is_empty() {
                ident.to_string()
            } else {
                format!("{prefix}::{ident}")
            }
        };
        match tree {
            syn::UseTree::Path(path) => flatten_use(&path.tree, &join(prefix, &path.ident), out),
            syn::UseTree::Name(name) if name.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Rename(rename) if rename.ident == "self" => {
                out.push(prefix.to_string());
            }
            syn::UseTree::Name(name) => out.push(join(prefix, &name.ident)),
            syn::UseTree::Rename(rename) => out.push(join(prefix, &rename.ident)),
            syn::UseTree::Glob(_) => out.push(join(prefix, &"*")),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    flatten_use(item, prefix, out);
                }
            }
        }
    }

    fn forbidden_reason(path: &str) -> Option<&'static str> {
        let path = path.strip_prefix("crate::").unwrap_or(path);
        let ends_with = |name: &str| path == name || path.ends_with(&format!("::{name}"));
        if path.starts_with("sea_orm") || ends_with("DatabaseConnection") {
            return Some("database dependency");
        }
        if path.starts_with("internal::ai::history") || ends_with("HistoryManager") {
            return Some("checkpoint history dependency");
        }
        if path.starts_with("internal::ai::traces") || ends_with("CheckpointCommit") {
            return Some("trace-ref dependency");
        }
        if path.starts_with("internal::ai::capture_scope") || ends_with("CaptureScope") {
            return Some("catalog scope dependency");
        }
        if path.starts_with("internal::ai::coverage_gate") || ends_with("CoverageGate") {
            return Some("catalog coverage dependency");
        }
        if path.starts_with("utils::client_storage") || ends_with("ClientStorage") {
            return Some("filesystem-store dependency");
        }
        if path.starts_with("git_internal")
            || path.starts_with("internal::protocol::git_client")
            || ends_with("Ref")
            || ends_with("RefName")
            || ends_with("Refspec")
        {
            return Some("git-ref dependency");
        }
        if path.starts_with("std::fs") || path.starts_with("std::path") {
            return Some("filesystem dependency");
        }
        None
    }

    struct IngressBoundaryGuard {
        violations: Vec<String>,
    }

    fn has_test_only_cfg(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if {
                    let condition = list.tokens.to_string().replace(' ', "");
                    condition == "test" || condition == "any(test,debug_assertions)"
                })
        })
    }

    impl<'ast> Visit<'ast> for IngressBoundaryGuard {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if has_test_only_cfg(&item.attrs) {
                return;
            }
            syn::visit::visit_item_mod(self, item);
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if has_test_only_cfg(&item.attrs) {
                return;
            }
            syn::visit::visit_item_fn(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = forbidden_reason(&path) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let joined = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(reason) = forbidden_reason(&joined) {
                self.violations.push(format!("path {joined} → {reason}"));
            }
            syn::visit::visit_path(self, path);
        }
    }

    let mut boundary_sources = vec![repo_root().join("src/internal/ai/capture/ingress.rs")];
    let providers_dir = repo_root().join("src/internal/ai/hooks/providers");
    for entry in fs::read_dir(&providers_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", providers_dir.display()))
    {
        let entry = entry.expect("read provider directory entry");
        if !entry
            .file_type()
            .expect("read provider directory entry type")
            .is_dir()
        {
            continue;
        }
        let parser_path = entry.path().join("parser.rs");
        assert!(
            parser_path.is_file(),
            "every provider directory must define a guarded parser.rs: {}",
            parser_path.display()
        );
        boundary_sources.push(parser_path);
    }
    let parser_count = boundary_sources.len() - 1;
    assert!(
        parser_count > 0,
        "provider parser boundary scan must not be empty"
    );
    for source_path in boundary_sources {
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", source_path.display()));
        let mut guard = IngressBoundaryGuard {
            violations: Vec::new(),
        };
        guard.visit_file(&file);
        assert!(
            guard.violations.is_empty(),
            "capture ingress/provider parser must remain store-free ({}):\n{}",
            source_path.display(),
            guard.violations.join("\n")
        );
    }

    const RUNTIME: &str = "src/internal/ai/hooks/runtime.rs";
    let runtime_path = repo_root().join(RUNTIME);
    let runtime = fs::read_to_string(&runtime_path).expect("read hook runtime source");
    let stdin_entry = anchored_section(
        &runtime,
        RUNTIME,
        "pub async fn process_hook_event_with_target(",
        "fn classify_capture_ingress_for_target(",
    );
    // ADR-ACF-10 (ACF-17): the AiIntent target is the moved legacy writer.
    let (_, runtime_ast) = parse_rust_source(RUNTIME);
    let mut runtime_imports = Vec::new();
    for item in &runtime_ast.items {
        if let syn::Item::Use(item) = item {
            flatten_syn_use(&item.tree, "", &mut runtime_imports);
        }
    }
    assert!(
        runtime_imports
            .iter()
            .any(|path| path == "super::intent::process_ai_intent_ingress"),
        "the AiIntent target must dispatch to hooks::intent::process_ai_intent_ingress: {runtime_imports:?}"
    );
    assert!(
        stdin_entry.contains("validate_capture_ingress_from_stdin("),
        "the production stdin dispatcher must validate before resolving scope/store"
    );
    assert!(
        !stdin_entry.contains("CaptureIngressCommand::from_payload")
            && !stdin_entry.contains("&[u8]"),
        "the production stdin dispatcher must never hold a raw hook frame"
    );
    let validate_at = stdin_entry
        .find("validate_capture_ingress_from_stdin(")
        .expect("shared stdin dispatcher validates ingress");
    let dispatch_at = stdin_entry
        .find("match target")
        .expect("shared stdin dispatcher selects target");
    assert!(
        validate_at < dispatch_at,
        "both hook targets must dispatch only after ingress validation"
    );
    for target_handoff in [
        "HookTarget::AiIntent => process_ai_intent_ingress(ingress_command, provider)",
        // ADR-ACF-10 (ACF-18): AgentTraces receives the callback's one
        // resolved live-capture binding instead of the raw provider.
        "ingest_agent_traces(ingress_command, binding, &ingest_span)",
    ] {
        assert!(
            stdin_entry.contains(target_handoff),
            "target dispatch must consume the same validated ingress command: {target_handoff}"
        );
    }

    let (runtime_production, _) = runtime
        .split_once("#[cfg(test)]\npub(crate) mod tests")
        .expect("runtime test module delimiter exists");
    // ADR-ACF-10 (ACF-19): the validated persistence handoff moved to the
    // live pipeline; its out-of-line test module is the production end.
    const LIVE_PIPELINE: &str = "src/internal/ai/capture/live_pipeline.rs";
    const LIVE_PIPELINE_TESTS: &str =
        "#[cfg(test)]\n#[path = \"live_pipeline_tests.rs\"]\nmod tests;";
    let live_pipeline =
        fs::read_to_string(repo_root().join(LIVE_PIPELINE)).expect("read live pipeline source");
    let (live_pipeline_production, _) = live_pipeline
        .split_once(LIVE_PIPELINE_TESTS)
        .unwrap_or_else(|| {
            panic!("{LIVE_PIPELINE}: test module `{LIVE_PIPELINE_TESTS}` is missing")
        });
    for (file, production) in [
        (RUNTIME, runtime_production),
        (LIVE_PIPELINE, live_pipeline_production),
    ] {
        assert!(
            !production.contains("CaptureIngressCommand::from_payload")
                && !production.contains("pub async fn ingest_agent_traces_payload("),
            "{file}: production capture must never accept or parse a raw hook payload"
        );
    }
    let test_support_path = repo_root().join("src/internal/ai/capture/test_support.rs");
    let test_support =
        fs::read_to_string(&test_support_path).expect("read in-process capture test support");
    assert!(
        !test_support.contains("&[u8]")
            && !test_support.contains("CaptureIngressCommand::from_payload"),
        "capture test support must receive only typed ingress outcomes, never raw hook frames"
    );
    assert!(
        test_support.contains("CaptureIngressOutcome"),
        "capture test support must consume the typed ingress outcome"
    );
    let ingress_path = repo_root().join("src/internal/ai/capture/ingress.rs");
    let ingress = fs::read_to_string(&ingress_path).expect("read capture ingress source");
    assert!(
        ingress.contains("#[cfg(any(test, debug_assertions))]\n#[doc(hidden)]\npub fn lower_in_process_capture_frame_for_test(")
            && ingress.contains("CaptureIngressCommand::from_payload"),
        "the integration-test raw frame exception must remain inside debug/test capture ingress"
    );
    assert!(
        !ingress.contains("hooks::runtime"),
        "capture ingress must not depend on the runtime while lowering raw test frames"
    );

    let stdin_validation_block = anchored_section(
        &runtime,
        RUNTIME,
        "async fn validate_capture_ingress_from_stdin(",
        "#[cfg(test)]\npub(crate) mod tests",
    );
    assert!(
        stdin_validation_block.contains("CaptureIngressCommand::from_stdin"),
        "production stdin validation must let capture ingress own raw frame reads"
    );
    assert!(
        !stdin_validation_block.contains("CaptureIngressCommand::from_payload"),
        "production stdin validation must not expose raw payload parsing in runtime"
    );
    assert!(
        stdin_validation_block.contains("deadline,"),
        "managed hook deadline must be forwarded through production ingress"
    );
    assert!(
        !stdin_validation_block.contains("provider,\n        None,"),
        "production ingress must not discard the provider-owned capture deadline"
    );

    let persistence_prefix = anchored_section(
        &live_pipeline,
        LIVE_PIPELINE,
        "async fn ingest_agent_traces_payload_with_scope(",
        LIVE_PIPELINE_TESTS,
    );
    assert!(
        persistence_prefix.contains("ingress_command: Box<CaptureIngressCommand>,")
            && persistence_prefix.contains(".into_parts();"),
        "{LIVE_PIPELINE}: the persistence helper must consume the validated ingress command"
    );
    assert!(
        !persistence_prefix.contains("CaptureIngressCommand::from_payload"),
        "the persistence helper must accept the already-validated ingress command"
    );
}

/// Linux epoll cannot register regular files, and a filesystem read already
/// running in Tokio's blocking pool cannot be cancelled at the hook deadline.
/// Keep this path behind the private killable helper rather than regressing to
/// an in-process blocking read that can hold shutdown indefinitely.
#[test]
fn regular_file_hook_stdin_uses_killable_helper() {
    use syn::visit::Visit;

    #[derive(Default)]
    struct BlockingTaskCallGuard {
        calls: Vec<String>,
    }

    impl<'ast> Visit<'ast> for BlockingTaskCallGuard {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref() {
                let rendered = syn_path_text(&path.path);
                if rendered.rsplit("::").next() == Some("spawn_blocking") {
                    self.calls.push(rendered);
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    let ingress_path = repo_root().join("src/internal/ai/capture/ingress.rs");
    let ingress = fs::read_to_string(&ingress_path).expect("read capture ingress source");
    let regular_reader = ingress
        .split("async fn read_regular_stdin_until_deadline(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("#[cfg(all(test, unix))]\nasync fn with_regular_stdin_helper_start")
                .next()
        })
        .expect("regular-file stdin helper exists");

    for required in [
        "AUTHORIZED_READ_HELPER_ARG",
        "AUTHORIZED_READ_HELPER_CAP_ENV",
        ".kill_on_drop(true)",
        "configure_private_helper_process_group(&mut command)",
        "CancellationSafeChild::new_process_group",
        "child.register_abort_on_cancel(&stdout_task)",
        "child.terminate_and_reap()",
    ] {
        assert!(
            regular_reader.contains(required),
            "regular-file stdin reader must retain killable helper behavior: {required}"
        );
    }
    let (_, ingress_file) = parse_rust_source("src/internal/ai/capture/ingress.rs");
    let regular_reader_function =
        top_level_function(&ingress_file, "read_regular_stdin_until_deadline");
    let mut blocking_calls = BlockingTaskCallGuard::default();
    blocking_calls.visit_item_fn(regular_reader_function);
    assert!(
        blocking_calls.calls.is_empty(),
        "regular-file stdin reader must not invoke an uncancellable Tokio blocking task: {}",
        blocking_calls.calls.join(", ")
    );
    assert!(
        regular_reader
            .find("let output = match deadline")
            .zip(regular_reader.find("let status = match child.child_mut"))
            .is_some_and(|(output, status)| output < status),
        "regular-file stdin reader must drain the helper stdout before waiting for its group leader"
    );

    let main_path = repo_root().join("src/main.rs");
    let main = fs::read_to_string(&main_path).expect("read main binary entrypoint");
    assert!(
        main.contains("authorized_read::register_running_program();"),
        "the running Libra binary must register itself so renamed --binary-path hooks can use the private reader"
    );
}

/// Child transcript discovery and projection are both potentially large
/// provider-controlled operations. Keep them outside the deadline-owning
/// runtime process, and prevent a future refactor from adding a second raw
/// JSONL parser after the discovery helper has already classified it.
#[test]
fn subagent_content_deadline_helpers_stay_killable_and_memory_bounded() {
    let path = repo_root().join("src/internal/ai/subagent_content.rs");
    let source = fs::read_to_string(&path).expect("read subagent content source");
    let production = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("subagent content production boundary exists");

    let discovery = production
        .split("pub(crate) async fn discover_claude_subagent_contents_bounded(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("/// Stop and retain a child-content helper")
                .next()
        })
        .expect("bounded subagent discovery helper exists");
    for required in [
        "let Some(helper_program) = helper_program()",
        ".arg(SUBAGENT_DISCOVERY_HELPER_ARG)",
        ".kill_on_drop(true)",
        "configure_private_helper_process_group(&mut command)",
        "CancellationSafeChild::new_process_group",
        "read_async_strictly_bounded(&mut stdout, output_cap)",
        "child.register_abort_on_cancel(&output_task)",
        "child.terminate_and_reap()",
        "decode_subagent_discovery_helper_frame(output, byte_budget, source_limit, deadline)",
    ] {
        assert!(
            discovery.contains(required),
            "{} must retain a registered, killable bounded discovery helper: {required}",
            path.display()
        );
    }
    let helper_program = production
        .split("fn helper_program() -> Option<PathBuf>")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("fn subagent_discovery_helper_output_cap")
                .next()
        })
        .expect("subagent helper program resolver exists");
    assert!(
        helper_program.contains("authorized_read::helper_program()")
            && !helper_program.contains("current_exe")
            && !helper_program.contains("debug_dir"),
        "{} must reuse the registered Libra executable rather than infer a sibling helper binary",
        path.display()
    );
    assert!(
        discovery
            .find("let output = match tokio::time::timeout_at")
            .zip(discovery.find("let status_result = match child.child_mut"))
            .is_some_and(|(output, status)| output < status),
        "{} must drain discovery stdout before waiting for the group leader",
        path.display()
    );
    assert!(
        !discovery.contains("wait_with_output")
            && !discovery.contains("subagent_source_completeness(&bytes, Some(deadline))"),
        "{} must drain stdout concurrently and must not reparse raw child JSONL in the deadline-owning parent",
        path.display()
    );
    let discovery_frame = production
        .split("fn decode_subagent_discovery_helper_frame(")
        .nth(1)
        .and_then(|entry| entry.split("/// Killable async boundary").next())
        .expect("direct-raw discovery frame decoder exists");
    for required in [
        "SUBAGENT_DISCOVERY_HELPER_HEADER_CAP",
        "raw_bytes != bytes_read",
        "header_end != output.len()",
        "bytes.extend_from_slice(&output[offset..end])",
        "SubagentDiscoveryHelperResponse::Error { deadline_exceeded }",
    ] {
        assert!(
            discovery_frame.contains(required),
            "{} must retain bounded direct-raw discovery framing: {required}",
            path.display()
        );
    }
    assert!(
        !production.contains("bytes_base64"),
        "{} must not restore a JSON/base64 child-transcript response that exceeds ACF-03's parent working-set budget",
        path.display()
    );

    let projection = production
        .split("async fn safe_content_projection_until(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("fn subagent_content_identity_digest_bytes")
                .next()
        })
        .expect("killable child projection helper exists");
    for required in [
        ".arg(SUBAGENT_PROJECTION_HELPER_ARG)",
        ".kill_on_drop(true)",
        "configure_private_helper_process_group(&mut command)",
        "CancellationSafeChild::new_process_group",
        "subagent_projection_response_cap(source_len)",
        "read_async_strictly_bounded(&mut stdout, output_cap)",
        "child.register_abort_on_cancel(&output_task)",
        "child.terminate_and_reap()",
        "decode_subagent_projection_frame(output, source_len)",
    ] {
        assert!(
            projection.contains(required),
            "{} must retain the capped child projection helper boundary: {required}",
            path.display()
        );
    }
    assert!(
        !projection.contains("read_to_end"),
        "{} must use the strict capped reader rather than a geometrically growing helper-output read",
        path.display()
    );
    assert!(
        projection
            .find("let output =")
            .zip(projection.find("let status_result = match child.child_mut"))
            .is_some_and(|(output, status)| output < status),
        "{} must drain projection stdout before waiting for the group leader",
        path.display()
    );
    let decoder = production
        .split("fn decode_subagent_projection_frame(")
        .nth(1)
        .and_then(|entry| entry.split("/// Run the CPU-heavy projection").next())
        .expect("subagent projection frame decoder exists");
    assert!(
        decoder.contains("frame.copy_within(") && !decoder.contains(".to_vec()"),
        "{} must compact the bounded helper response in place rather than clone a full child projection",
        path.display()
    );

    let prepare = production
        .split("async fn capture_discovered_subagent_contents_inner(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("async fn build_unchanged_durability_proof")
                .next()
        })
        .expect("subagent capture preparation exists");
    assert!(
        prepare.contains("safe_content_projection_until(source, projection_deadline).await?"),
        "{} must use the async killable projection helper before persisting child content",
        path.display()
    );

    let main_path = repo_root().join("src/main.rs");
    let main = fs::read_to_string(&main_path).expect("read main binary entrypoint");
    assert!(
        main.contains("run_subagent_projection_helper_if_requested()")
            && main.contains("SUBAGENT_PROJECTION_HELPER_ARG"),
        "the main Libra binary must dispatch the private subagent projection helper before normal CLI startup"
    );
}

/// Import discovery, preparation, and object-index repair each run private
/// helpers. Keep their executable provenance and cancellation rules aligned
/// with the descriptor-reader boundary: an embedded host must never receive
/// a private Libra argv, and a helper that retains stdout must honor its
/// caller-supplied deadline once the helper phase starts. This guard does not
/// claim a strict end-to-end source-acquisition deadline: the parent currently
/// resolves and pins a source before descriptor handoff.
#[test]
fn import_command_helpers_stay_registered_bounded_and_content_free() {
    let path = repo_root().join("src/command/agent/import.rs");
    let source = fs::read_to_string(&path).expect("read import command source");
    let production = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("import command production boundary exists");
    assert!(
        !production.contains("std::env::current_exe") && !production.contains("wait_with_output"),
        "{} must use the registered bounded-helper lifecycle rather than infer or wait on a host executable",
        path.display()
    );

    let discovery = production
        .split("async fn discover_bounded(")
        .nth(1)
        .and_then(|entry| entry.split("#[cfg(unix)]\nfn wait_for_consent_fd").next())
        .expect("bounded import discovery helper exists");
    let index_repair = production
        .split("async fn invoke_import_index_repair_helper(")
        .nth(1)
        .and_then(|entry| entry.split("fn parse_import_index_barrier_marker").next())
        .expect("bounded import index-repair helper exists");
    for (name, helper) in [("discovery", discovery), ("index repair", index_repair)] {
        for required in [
            "registered_helper_command(",
            "require_registered_import_helper_command(",
            "run_registered_bounded_helper_until(",
            "RegisteredHelperOutput::DeadlineExceeded",
        ] {
            assert!(
                helper.contains(required),
                "{} import {name} helper must retain registered bounded execution: {required}",
                path.display()
            );
        }
    }
    assert!(
        index_repair.contains("IndexRepairHelperResponse::Error {}")
            && !index_repair.contains("format!(\"{error:#}\")"),
        "{} index-repair helper must not return a provider/database error chain",
        path.display()
    );

    // The preparation protocol deliberately has no request frame on stdin:
    // stdin is the already-authorized, parent-pinned descriptor.  Its small
    // JSON control frame contains only commitments and fixed bounds.
    let preparation = production
        .split("async fn run_import_preparation_descriptor_helper_bounded(")
        .nth(1)
        .and_then(|entry| entry.split("async fn prepare_candidate_bounded(").next())
        .expect("descriptor-owning import preparation helper runner exists");
    for required in [
        "Stdio::from(source)",
        "configure_private_helper_process_group(&mut command)",
        "CancellationSafeChild::new_process_group(child)",
        "child.register_abort_on_cancel(&stdout_task)",
        "read_async_strictly_bounded(&mut stdout, output_cap)",
        "child.disarm_child_after_wait()",
    ] {
        assert!(
            preparation.contains(required),
            "{} descriptor preparation helper must retain its bounded process-group lifecycle: {required}",
            path.display()
        );
    }
    assert!(
        preparation.contains("#[cfg(not(unix))]")
            && preparation.contains("ImportError::AuthorizedReaderUnavailable"),
        "{} descriptor preparation must fail closed outside Unix rather than launch a raw-source helper without process-tree containment",
        path.display()
    );
    let stdout_drain = preparation
        .find("let mut stdout_task")
        .expect("descriptor preparation starts a capped stdout drain");
    let leader_wait = preparation
        .find("child_process.wait()")
        .expect("descriptor preparation reaps its leader after pipe drain");
    let disarm = preparation
        .find("child.disarm_child_after_wait()")
        .expect("descriptor preparation disarms its process group after the leader wait");
    assert!(
        stdout_drain < leader_wait && leader_wait < disarm,
        "{} descriptor preparation must drain inherited stdout before reaping/disarming its process-group leader",
        path.display()
    );

    let candidate_preparation = production
        .split("async fn prepare_candidate_bounded(")
        .nth(1)
        .and_then(|entry| entry.split("fn stable_code_for_error(").next())
        .expect("descriptor preparation caller exists");
    for required in [
        "registered_helper_command(",
        "require_registered_import_helper_command(",
        "IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG",
        "run_import_preparation_descriptor_helper_bounded(",
    ] {
        assert!(
            candidate_preparation.contains(required),
            "{} import preparation must use the registered Libra descriptor helper: {required}",
            path.display()
        );
    }

    let (_, command_file) = parse_rust_source("src/command/agent/import.rs");
    let descriptor_control = top_level_struct(&command_file, "PreparationDescriptorControl");
    let control_fields = descriptor_control
        .fields
        .iter()
        .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
        .collect::<BTreeSet<_>>();
    let permitted_control_fields = BTreeSet::from([
        "agent_kind".to_string(),
        "source_kind".to_string(),
        "provider_commitment".to_string(),
        // A future descriptor protocol may carry the separately domain-bound
        // transient source preimage. It remains safe only as a fixed-size
        // commitment, never as a raw locator.
        "source_preimage".to_string(),
        "read_cap".to_string(),
        "remaining_ms".to_string(),
    ]);
    assert!(
        control_fields.is_subset(&permitted_control_fields)
            && control_fields.contains("agent_kind")
            && control_fields.contains("source_kind")
            && control_fields.contains("read_cap")
            && control_fields.contains("remaining_ms")
            && (control_fields.contains("provider_commitment")
                || control_fields.contains("source_preimage")),
        "{} descriptor control must contain only validated kinds/bounds and fixed commitments, never raw locators, provider IDs, paths, or existing-session metadata; got {control_fields:?}",
        path.display()
    );
    for field in descriptor_control.fields.iter().filter(|field| {
        field.ident.as_ref().is_some_and(|name| {
            name.to_string().contains("commitment") || name == "source_preimage"
        })
    }) {
        let fixed_commitment = matches!(
            &field.ty,
            syn::Type::Array(array)
                if matches!(
                    array.elem.as_ref(),
                    syn::Type::Path(path) if path.path.is_ident("u8")
                ) && matches!(
                    &array.len,
                    syn::Expr::Lit(length)
                        if matches!(&length.lit, syn::Lit::Int(value) if value.base10_digits() == "32")
                )
        );
        assert!(
            fixed_commitment,
            "{} descriptor control field {:?} must be a fixed 32-byte commitment, never a serialized provider identifier",
            path.display(),
            field.ident
        );
    }
    assert!(
        descriptor_control.attrs.iter().any(|attribute| {
            attribute.path().is_ident("serde")
                && matches!(&attribute.meta, syn::Meta::List(list) if list.tokens.to_string().contains("deny_unknown_fields"))
        }),
        "{} descriptor control must reject unknown fields rather than silently accept a future raw metadata field",
        path.display()
    );
    // A discovery helper reports failure only as a closed, payload-free
    // reason; the parent renders the fixed actionable message for it.
    let top_level_enum = |name: &str| {
        command_file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Enum(item) if item.ident == name => Some(item),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected top-level enum {name}"))
    };
    let discovery_error = top_level_enum("DiscoveryHelperResponse")
        .variants
        .iter()
        .find(|variant| variant.ident == "Error")
        .expect("bounded discovery response has an error variant");
    assert!(
        matches!(&discovery_error.fields, syn::Fields::Named(fields)
            if fields.named.len() == 1
                && fields.named[0].ident.as_ref().is_some_and(|name| name == "reason")
                && matches!(&fields.named[0].ty, syn::Type::Path(ty) if ty.path.is_ident("DiscoveryRejection")))
            && top_level_enum("DiscoveryRejection")
                .variants
                .iter()
                .all(|variant| matches!(variant.fields, syn::Fields::Unit)),
        "{} discovery helper errors must cross the wire only as a closed unit-variant reason",
        path.display()
    );

    let main_path = repo_root().join("src/main.rs");
    let main = fs::read_to_string(&main_path).expect("read main binary entrypoint");
    assert!(
        main.contains("run_import_preparation_descriptor_helper_if_requested()")
            && main.contains("IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG"),
        "the main Libra binary must dispatch the registered descriptor preparation helper before normal CLI startup"
    );

    let candidate_source = production
        .split("async fn resolve_candidate_source(")
        .nth(1)
        .and_then(|entry| entry.split("fn preparation_error_kind").next())
        .expect("OpenCode import source resolver exists");
    assert!(
        candidate_source.contains("authorized_trusted_sandboxed_export_until(")
            && candidate_source.contains("deadline,")
            && !candidate_source.contains("trusted_opencode_binary"),
        "{} OpenCode import must share its absolute deadline with trust revalidation and export",
        path.display()
    );
}

/// Once a live coverage reservation begins, it retains the caller-supplied
/// deadline rather than merely applying a source-reader timeout. In particular,
/// an all-covered terminal replay must not consume the receipt after coverage
/// work has run past the original window. This guard is deliberately limited to
/// reservation and terminal handling; it does not claim a strict whole-hook
/// deadline before descriptor handoff.
#[test]
fn live_coverage_reservation_propagates_deadline_and_never_acknowledges_late_terminal() {
    let coverage_path = repo_root().join("src/internal/ai/coverage_gate.rs");
    let coverage = fs::read_to_string(&coverage_path).expect("read coverage gate source");
    let live_until = coverage
        .split("pub(crate) async fn reserve_live_turn_claims_until(")
        .nth(1)
        .and_then(|entry| entry.split("/// [`reserve_live_turn_claims").next())
        .expect("deadline-aware live coverage reservation exists");
    let live_execution = coverage
        .split("pub(crate) const fn live(deadline: CaptureCommitDeadline) -> Self")
        .nth(1)
        .and_then(|entry| entry.split("pub(crate) const fn export").next())
        .expect("live deadline execution wrapper exists");
    let deadline_aware_reservation = coverage
        .split("pub(crate) async fn reserve_turn_claims_for_channel_with_capture_scope_until(")
        .nth(1)
        .and_then(|entry| entry.split("/// [`reserve_live_turn_claims").next())
        .expect("deadline-aware scoped coverage reservation exists");
    assert!(
        live_until.contains("CaptureReservationExecution::live(deadline)")
            && live_execution.contains("deadline,")
            && deadline_aware_reservation.contains("Some(execution.deadline)")
            && !live_until.contains(".chunks("),
        "{} must retain one atomic live reservation transaction with its caller-supplied deadline propagated",
        coverage_path.display()
    );

    // ADR-ACF-10 (ACF-18): the live coverage stage is the extracted
    // `reserve_live_coverage` in the live checkpoint module. Its syn span is
    // scanned, so neither a neighbouring item nor a moved text delimiter can
    // satisfy (or silently widen) the assertions below.
    use syn::spanned::Spanned;

    const LIVE_CHECKPOINT: &str = "src/internal/ai/capture/live_checkpoint.rs";
    let checkpoint_path = repo_root().join(LIVE_CHECKPOINT);
    let live_checkpoint =
        fs::read_to_string(&checkpoint_path).expect("read live checkpoint source");
    let live_checkpoint_file =
        syn::parse_file(&live_checkpoint).expect("parse live checkpoint source");
    let coverage_stage = span_source_text(
        &live_checkpoint,
        top_level_function(&live_checkpoint_file, "reserve_live_coverage").span(),
    );
    for required in [
        "coverage_gate::reserve_live_turn_claims_until(",
        "coverage_gate::abandon_reserved_turn_claims_with_capture_scope_until(",
        "release elapsed-deadline live coverage reservation",
        "settle_preapplied_without_checkpoint(",
    ] {
        assert!(
            coverage_stage.contains(required),
            "{} reserve_live_coverage must retain deadline-safe live coverage cleanup: {required}",
            checkpoint_path.display()
        );
    }
    let deadline_at = coverage_stage
        .find("Instant::now() >= deadline.monotonic()")
        .expect("live coverage deadline check exists");
    let normalize_at = coverage_stage
        .find("normalize(transcript_redacted)")
        .expect("live coverage normalizer call exists");
    assert!(
        deadline_at < normalize_at,
        "{} reserve_live_coverage must observe the capture deadline before starting the synchronous normalizer",
        checkpoint_path.display()
    );
    let cleanup_at = coverage_stage
        .find("release elapsed-deadline live coverage reservation")
        .expect("deadline cleanup context exists");
    let noop_at = coverage_stage
        .find("if outcome.is_noop()")
        .expect("coverage no-op branch exists");
    assert!(
        cleanup_at < noop_at,
        "{} must resolve an elapsed deadline before any coverage no-op can acknowledge a terminal receipt",
        checkpoint_path.display()
    );
    let writer = span_source_text(
        &live_checkpoint,
        top_level_function(&live_checkpoint_file, "write_committed_checkpoint").span(),
    );
    assert!(
        writer.contains("binding.live_coverage_normalizer()")
            && writer.contains("reserve_live_coverage("),
        "{} write_committed_checkpoint must route the provider's live coverage normalizer through reserve_live_coverage",
        checkpoint_path.display()
    );
}

/// Repository-relative paths of every Rust source under `relative_dir`,
/// sorted so guard diagnostics are deterministic.
fn rust_sources_under(relative_dir: &str) -> Vec<String> {
    let mut stack = vec![repo_root().join(relative_dir)];
    let mut sources = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        {
            let path = entry.expect("read source directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(
                    path.strip_prefix(repo_root())
                        .expect("scanned source lives under the repository")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    sources.sort();
    sources
}

/// Outer attributes of a statement, so an exact `#[cfg(test)]` seam inside a
/// production function is treated as test-only.
fn syn_stmt_attrs(stmt: &syn::Stmt) -> &[syn::Attribute] {
    match stmt {
        syn::Stmt::Local(local) => &local.attrs,
        syn::Stmt::Item(item) => syn_item_attrs(item),
        syn::Stmt::Macro(mac) => &mac.attrs,
        syn::Stmt::Expr(expr, _) => match expr {
            syn::Expr::Assign(expr) => &expr.attrs,
            syn::Expr::Await(expr) => &expr.attrs,
            syn::Expr::Block(expr) => &expr.attrs,
            syn::Expr::Call(expr) => &expr.attrs,
            syn::Expr::If(expr) => &expr.attrs,
            syn::Expr::Macro(expr) => &expr.attrs,
            syn::Expr::MethodCall(expr) => &expr.attrs,
            syn::Expr::Path(expr) => &expr.attrs,
            syn::Expr::Try(expr) => &expr.attrs,
            _ => &[],
        },
    }
}

/// ADR-ACF-10 provider token in a string literal: a segment (split on every
/// non-alphanumeric character, `_` and `-` included) equal to a builtin
/// provider name, or the exact literal `cursor` / `factory_ai`; compared
/// case-insensitively.
fn literal_has_provider_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    matches!(lower.as_str(), "cursor" | "factory_ai")
        || lower
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|segment| {
                matches!(
                    segment,
                    "claude" | "codex" | "opencode" | "gemini" | "copilot" | "pi"
                )
            })
}

/// ADR-ACF-10 provider token in an identifier: a builtin provider name as a
/// substring (so `opencode_export` and `ClaudeLiveCapture` both count), or a
/// `pi` segment.
fn identifier_has_provider_token(identifier: &str) -> bool {
    let lower = identifier.to_ascii_lowercase();
    ["claude", "codex", "opencode", "gemini", "copilot"]
        .iter()
        .any(|name| lower.contains(name))
        || lower
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|segment| segment == "pi")
}

/// Non-test provider tokens of one parsed file: string literals and
/// identifiers, macro token trees included, attributes (docs) excluded. A
/// provider-to-kind dispatch name (`AgentKind`, `agent_for`,
/// `live_capture_for`, `from_db_str`, `from_cli_slug`) is a hit as well.
#[derive(Default)]
struct ProviderTokenScan {
    hits: Vec<(proc_macro2::LineColumn, String)>,
}

impl ProviderTokenScan {
    fn literal(&mut self, value: &str, span: proc_macro2::Span) {
        if literal_has_provider_token(value) {
            self.hits.push((span.start(), format!("literal {value:?}")));
        }
    }

    fn identifier(&mut self, identifier: &proc_macro2::Ident) {
        let text = identifier.to_string();
        let text = text.strip_prefix("r#").unwrap_or(&text);
        if identifier_has_provider_token(text)
            || matches!(
                text,
                "AgentKind" | "agent_for" | "live_capture_for" | "from_db_str" | "from_cli_slug"
            )
        {
            self.hits
                .push((identifier.span().start(), format!("identifier {text}")));
        }
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Ident(identifier) => self.identifier(&identifier),
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                        self.literal(&value.value(), literal.span());
                    }
                }
                proc_macro2::TokenTree::Punct(_) => {}
            }
        }
    }

    /// Hits outside `exempt` regions, counted per source line.
    fn lines_outside(
        &self,
        exempt: &[(proc_macro2::LineColumn, proc_macro2::LineColumn)],
    ) -> BTreeSet<usize> {
        let position = |at: proc_macro2::LineColumn| (at.line, at.column);
        self.hits
            .iter()
            .filter(|(at, _)| {
                !exempt.iter().any(|(start, end)| {
                    position(*start) <= position(*at) && position(*at) <= position(*end)
                })
            })
            .map(|(at, _)| at.line)
            .collect()
    }
}

impl<'ast> syn::visit::Visit<'ast> for ProviderTokenScan {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !has_exact_cfg_test(syn_item_attrs(item)) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    // Attribute and doc text is prose, never a dispatch decision.
    fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

    fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
        self.identifier(identifier);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.literal(&literal.value(), literal.span());
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        syn::visit::visit_macro(self, mac);
        self.tokens(mac.tokens.clone());
    }
}

/// ADR-ACF-10 (ACF-18) "Typed identity", "Port types", "Port ownership" and
/// the provider-port ban:
/// - every `LiveCaptureProvider` / `LiveSubagentDiscovery` implementation
///   and every production `LiveTranscriptExporter` lives under
///   `observed_agents/`, and the port carries exactly the ACF-18 methods plus
///   the ACF-20 `transcript_exporter`;
/// - `observed_agents::live_capture_for` is the only function returning a
///   `LiveCaptureProvider`, its `match` has no `_` arm, and its `Some` arms are
///   exactly the registry's hook-installable kinds;
/// - `LiveCaptureBinding::resolve` runs once in the hook entry and once in the
///   in-process harness, and builtin typed identities are declared together
///   in `hooks/providers/mod.rs`;
/// - `observed_agents/live_capture.rs` uses none of the ADR's banned
///   persistence/capture names (`subagent_content` limited to three names);
/// - `hooks/**` and `capture/{live,live_pipeline,live_checkpoint}.rs` hold no
///   `capture_agent_kind`, no `from_db_str(` / `from_cli_slug(` and no
///   `provider_name()`-keyed string dispatch;
/// - `capture/{live,live_pipeline,live_checkpoint}.rs` hold no provider token
///   in non-test literals or identifiers (ACF-20 retired the export-region
///   exemptions together with the OpenCode export code they covered).
///
/// Every scanner is self-tested on inline fixtures so the guard cannot pass
/// vacuously.
#[test]
fn live_capture_port_and_store_boundaries() {
    use syn::{spanned::Spanned, visit::Visit};

    // --- Port implementations, the single lookup, and binding resolution.
    #[derive(Default)]
    struct PortSiteVisitor {
        test_depth: usize,
        functions: Vec<String>,
        port_impls: Vec<(String, String, bool)>,
        identity_impls: Vec<(String, bool)>,
        provider_returning: Vec<String>,
        resolve_calls: Vec<String>,
    }

    fn type_name(ty: &syn::Type) -> String {
        match ty {
            syn::Type::Path(path) => syn_path_text(&path.path),
            _ => "<type>".to_string(),
        }
    }

    fn mentions_live_capture_provider(output: &syn::ReturnType) -> bool {
        struct Finder(bool);
        impl<'ast> Visit<'ast> for Finder {
            fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
                self.0 |= identifier == "LiveCaptureProvider";
            }
        }
        let mut finder = Finder(false);
        finder.visit_return_type(output);
        finder.0
    }

    impl PortSiteVisitor {
        fn function<F: FnOnce(&mut Self)>(
            &mut self,
            name: String,
            attrs: &[syn::Attribute],
            output: &syn::ReturnType,
            visit: F,
        ) {
            let test = has_exact_cfg_test(attrs);
            self.test_depth += usize::from(test);
            if self.test_depth == 0 && mentions_live_capture_provider(output) {
                self.provider_returning.push(name.clone());
            }
            self.functions.push(name);
            visit(self);
            self.functions.pop();
            self.test_depth -= usize::from(test);
        }
    }

    impl<'ast> Visit<'ast> for PortSiteVisitor {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let test = has_exact_cfg_test(syn_item_attrs(item));
            self.test_depth += usize::from(test);
            syn::visit::visit_item(self, item);
            self.test_depth -= usize::from(test);
        }

        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if let Some((_, path, _)) = &item.trait_
                && let Some(segment) = path.segments.last()
            {
                let trait_name = segment.ident.to_string();
                let self_type = type_name(&item.self_ty);
                if matches!(
                    trait_name.as_str(),
                    "LiveCaptureProvider" | "LiveSubagentDiscovery" | "LiveTranscriptExporter"
                ) {
                    self.port_impls
                        .push((trait_name, self_type, self.test_depth > 0));
                } else if trait_name == "HookProviderIdentity" {
                    self.identity_impls.push((self_type, self.test_depth > 0));
                }
            }
            syn::visit::visit_item_impl(self, item);
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            self.function(
                item.sig.ident.to_string(),
                &item.attrs,
                &item.sig.output,
                |this| {
                    syn::visit::visit_item_fn(this, item);
                },
            );
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            self.function(
                item.sig.ident.to_string(),
                &item.attrs,
                &item.sig.output,
                |this| {
                    syn::visit::visit_impl_item_fn(this, item);
                },
            );
        }

        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            self.function(
                item.sig.ident.to_string(),
                &item.attrs,
                &item.sig.output,
                |this| {
                    syn::visit::visit_trait_item_fn(this, item);
                },
            );
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if self.test_depth == 0
                && let syn::Expr::Path(path) = call.func.as_ref()
                && syn_path_text(&path.path).ends_with("LiveCaptureBinding::resolve")
            {
                self.resolve_calls.push(
                    self.functions
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "<module>".to_string()),
                );
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    let sources = rust_sources_under("src");
    assert!(
        sources.len() > 100,
        "the port-site scan must cover the crate sources, got {}",
        sources.len()
    );
    let mut port_impls = BTreeSet::new();
    let mut identity_impls = Vec::new();
    let mut provider_returning = Vec::new();
    let mut resolve_calls = Vec::new();
    for relative_path in &sources {
        let (_, file) = parse_rust_source(relative_path);
        let mut visitor = PortSiteVisitor::default();
        // Out-of-line `#[cfg(test)]` modules (`*_tests.rs`, `*_test.rs`,
        // `tests.rs`) carry no attribute of their own.
        let stem = relative_path.trim_end_matches(".rs");
        if stem.ends_with("tests") || stem.ends_with("_test") {
            visitor.test_depth = 1;
        }
        visitor.visit_file(&file);
        for (trait_name, self_type, test) in visitor.port_impls {
            // A test exporter may script the exporter port from a capture
            // test; every other implementation is provider-layer code.
            if test && trait_name == "LiveTranscriptExporter" {
                continue;
            }
            assert!(
                relative_path.starts_with("src/internal/ai/observed_agents/"),
                "{relative_path}: `impl {trait_name} for {self_type}` must live in the observed_agents provider layer"
            );
            port_impls.insert(format!("{trait_name} for {self_type}"));
        }
        identity_impls.extend(
            visitor
                .identity_impls
                .into_iter()
                .map(|(self_type, test)| (relative_path.clone(), self_type, test)),
        );
        provider_returning.extend(
            visitor
                .provider_returning
                .into_iter()
                .map(|name| format!("{relative_path}::{name}")),
        );
        resolve_calls.extend(
            visitor
                .resolve_calls
                .into_iter()
                .map(|name| format!("{relative_path}::{name}")),
        );
    }
    for expected in [
        "LiveCaptureProvider for NeutralLiveCapture",
        "LiveCaptureProvider for ClaudeLiveCapture",
        "LiveCaptureProvider for CodexLiveCapture",
        "LiveCaptureProvider for OpenCodeLiveCapture",
        "LiveSubagentDiscovery for ClaudeLiveCapture",
        "LiveTranscriptExporter for OpenCodeLiveCapture",
    ] {
        assert!(
            port_impls.contains(expected),
            "expected `impl {expected}` in observed_agents: {port_impls:?}"
        );
    }
    assert_eq!(
        provider_returning,
        ["src/internal/ai/observed_agents/mod.rs::live_capture_for"],
        "observed_agents::live_capture_for must be the only lookup that returns a LiveCaptureProvider"
    );
    assert_eq!(
        resolve_calls,
        [
            "src/internal/ai/capture/test_support.rs::ingest_agent_traces_ingress_outcome_for_test",
            "src/internal/ai/hooks/runtime.rs::process_hook_event_with_target",
        ],
        "LiveCaptureBinding::resolve is the hook entry's single provider lookup (plus the in-process harness)"
    );
    let production_identities: BTreeSet<(String, String)> = identity_impls
        .iter()
        .filter(|(_, _, test)| !test)
        .map(|(path, self_type, _)| (path.clone(), self_type.clone()))
        .collect();
    let builtin_identities: BTreeSet<(String, String)> = [
        "claude::ClaudeProvider",
        "codex::CodexProvider",
        "gemini::GeminiProvider",
        "opencode::OpenCodeProvider",
    ]
    .into_iter()
    .map(|self_type| {
        (
            "src/internal/ai/hooks/providers/mod.rs".to_string(),
            self_type.to_string(),
        )
    })
    .collect();
    assert_eq!(
        production_identities, builtin_identities,
        "the builtin typed identities must be declared together in hooks/providers/mod.rs"
    );

    // The ACF-18 port surface plus the ACF-20 transcript exporter.
    let (_, live_capture_file) =
        parse_rust_source("src/internal/ai/observed_agents/live_capture.rs");
    let port_methods: BTreeSet<String> = live_capture_file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Trait(port) if port.ident == "LiveCaptureProvider" => Some(port),
            _ => None,
        })
        .expect("observed_agents/live_capture.rs defines the LiveCaptureProvider port")
        .items
        .iter()
        .filter_map(|item| match item {
            syn::TraitItem::Fn(method) => Some(method.sig.ident.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        port_methods,
        BTreeSet::from(
            [
                "live_coverage_normalizer",
                "live_transcript_candidate",
                "missing_host_capture_budget",
                "subagent_discovery",
                "transcript_exporter",
            ]
            .map(String::from)
        ),
        "LiveCaptureProvider carries exactly the ACF-18 capabilities and the ACF-20 exporter"
    );

    // `live_capture_for`: one exhaustive match, no `_` arm, `Some` exactly
    // for the hook-installable kinds.
    let (_, observed_mod) = parse_rust_source("src/internal/ai/observed_agents/mod.rs");
    let lookup = top_level_function(&observed_mod, "live_capture_for");
    let Some(syn::Stmt::Expr(syn::Expr::Match(lookup_match), None)) = lookup.block.stmts.last()
    else {
        panic!("live_capture_for must end in its exhaustive kind match");
    };
    fn wildcard(pattern: &syn::Pat) -> bool {
        match pattern {
            syn::Pat::Wild(_) => true,
            syn::Pat::Or(or) => or.cases.iter().any(wildcard),
            syn::Pat::Ident(binding) => binding.subpat.is_none(),
            _ => false,
        }
    }
    fn variants(pattern: &syn::Pat, output: &mut BTreeSet<String>) {
        match pattern {
            syn::Pat::Or(or) => or.cases.iter().for_each(|case| variants(case, output)),
            syn::Pat::Path(path) => {
                output.insert(syn_path_text(&path.path));
            }
            _ => {}
        }
    }
    let mut live_variants = BTreeSet::new();
    let mut neutral_variants = BTreeSet::new();
    for arm in &lookup_match.arms {
        assert!(
            !wildcard(&arm.pat),
            "live_capture_for must not have a `_` (or catch-all binding) arm"
        );
        let target = match arm.body.as_ref() {
            syn::Expr::Path(path) if path.path.is_ident("None") => &mut neutral_variants,
            syn::Expr::Call(call) if matches!(call.func.as_ref(), syn::Expr::Path(path) if path.path.is_ident("Some")) => {
                &mut live_variants
            }
            _ => panic!("live_capture_for arms must return Some(&STATIC) or None"),
        };
        variants(&arm.pat, target);
    }
    let installable: BTreeSet<String> = AgentKind::all()
        .iter()
        .filter(|kind| registration_for(**kind).hook_installable)
        .map(|kind| format!("AgentKind::{kind:?}"))
        .collect();
    let every_kind: BTreeSet<String> = AgentKind::all()
        .iter()
        .map(|kind| format!("AgentKind::{kind:?}"))
        .collect();
    assert_eq!(
        live_variants, installable,
        "live_capture_for must return Some exactly for the hook-installable kinds"
    );
    assert_eq!(
        live_variants
            .union(&neutral_variants)
            .cloned()
            .collect::<BTreeSet<_>>(),
        every_kind,
        "live_capture_for must name every AgentKind variant explicitly"
    );

    // --- Provider-port ban (whole file, tests included).
    #[derive(Default)]
    struct PortBan {
        violations: Vec<String>,
    }

    impl PortBan {
        fn path(&mut self, path: &str) {
            let segments: Vec<&str> = path.split("::").collect();
            let banned_segment = segments.iter().any(|segment| {
                matches!(
                    *segment,
                    "sea_orm"
                        | "history"
                        | "traces"
                        | "coverage_gate"
                        | "export_job"
                        | "capture"
                        | "CaptureScope"
                        | "CaptureCommitDeadline"
                        | "DatabaseConnection"
                        | "HistoryManager"
                )
            });
            let internal_db = segments.windows(2).any(|pair| pair == ["internal", "db"]);
            let subagent_content = segments
                .iter()
                .position(|segment| *segment == "subagent_content")
                .is_some_and(|at| {
                    !matches!(
                        segments.get(at + 1).copied(),
                        Some(
                            "SubagentDiscovery"
                                | "discover_claude_subagent_contents_bounded"
                                | "MAX_SUBAGENT_SOURCES_PER_CAPTURE"
                        )
                    )
                });
            if banned_segment || internal_db || subagent_content {
                self.violations.push(path.to_string());
            }
        }
    }

    impl<'ast> Visit<'ast> for PortBan {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                self.path(&path);
            }
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.path(&syn_path_text(path));
            syn::visit::visit_path(self, path);
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            fn identifiers(tokens: proc_macro2::TokenStream, ban: &mut PortBan) {
                for token in tokens {
                    match token {
                        proc_macro2::TokenTree::Group(group) => identifiers(group.stream(), ban),
                        proc_macro2::TokenTree::Ident(identifier) => {
                            ban.path(&identifier.to_string());
                        }
                        _ => {}
                    }
                }
            }
            syn::visit::visit_macro(self, mac);
            identifiers(mac.tokens.clone(), self);
        }
    }

    let port_ban = |source: &str| {
        let mut ban = PortBan::default();
        ban.visit_file(&syn::parse_file(source).expect("parse provider-port fixture"));
        ban.violations
    };
    let banned_fixture = port_ban(
        r#"
        use crate::internal::ai::capture::live::LiveExportRunner;
        use crate::internal::ai::subagent_content::{
            SubagentDiscovery, capture_discovered_subagent_contents_with_scope,
        };
        use crate::internal::ai::subagent_content;
        fn regressed(_: &sea_orm::DatabaseConnection, _: CaptureScope) {
            let _ = crate::internal::ai::export_job::release;
            let _ = crate::internal::db::get_db_conn_instance;
        }
        "#,
    );
    assert_eq!(
        banned_fixture.len(),
        7,
        "provider-port ban self-test: {banned_fixture:?}"
    );
    let allowed_fixture = port_ban(
        r#"
        use crate::internal::ai::subagent_content::{
            MAX_SUBAGENT_SOURCES_PER_CAPTURE, SubagentDiscovery,
            discover_claude_subagent_contents_bounded,
        };
        use super::{AgentKind, RedactedBytes, opencode_export::ExportLimits};
        fn allowed(deadline: std::time::Instant) -> Option<SubagentDiscovery> {
            tracing::warn!(reason = "fixed", "capture skipped");
            let _ = deadline;
            None
        }
        "#,
    );
    assert!(
        allowed_fixture.is_empty(),
        "provider-port ban self-test must accept the three allowed names: {allowed_fixture:?}"
    );
    let live_capture_source =
        fs::read_to_string(repo_root().join("src/internal/ai/observed_agents/live_capture.rs"))
            .expect("read observed_agents/live_capture.rs");
    let live_capture_violations = port_ban(&live_capture_source);
    assert!(
        live_capture_violations.is_empty(),
        "observed_agents/live_capture.rs must not use the ADR-ACF-10 provider-port banned names:\n{}",
        live_capture_violations.join("\n")
    );

    // --- No provider-name -> AgentKind mapping in the hook/capture layers.
    #[derive(Default)]
    struct NameMappingScan {
        hits: Vec<String>,
    }

    fn mentions_identifier(tokens: proc_macro2::TokenStream, name: &str) -> bool {
        tokens.into_iter().any(|token| match token {
            proc_macro2::TokenTree::Group(group) => mentions_identifier(group.stream(), name),
            proc_macro2::TokenTree::Ident(identifier) => identifier == name,
            _ => false,
        })
    }

    fn has_string_literal(tokens: proc_macro2::TokenStream) -> bool {
        tokens.into_iter().any(|token| match token {
            proc_macro2::TokenTree::Group(group) => has_string_literal(group.stream()),
            proc_macro2::TokenTree::Literal(literal) => {
                syn::parse_str::<syn::LitStr>(&literal.to_string()).is_ok()
            }
            _ => false,
        })
    }

    fn string_pattern(pattern: &syn::Pat) -> bool {
        match pattern {
            syn::Pat::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            }) => true,
            syn::Pat::Or(or) => or.cases.iter().any(string_pattern),
            _ => false,
        }
    }

    fn expr_mentions(expr: &syn::Expr, name: &str) -> bool {
        struct Finder<'n> {
            name: &'n str,
            found: bool,
        }
        impl<'ast> Visit<'ast> for Finder<'_> {
            fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
                self.found |= identifier == self.name;
            }
            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                syn::visit::visit_macro(self, mac);
                self.found |= mentions_identifier(mac.tokens.clone(), self.name);
            }
        }
        let mut finder = Finder { name, found: false };
        finder.visit_expr(expr);
        finder.found
    }

    fn string_literal(expr: &syn::Expr) -> bool {
        matches!(
            expr,
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            })
        )
    }

    impl<'ast> Visit<'ast> for NameMappingScan {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if !has_exact_cfg_test(syn_item_attrs(item)) {
                syn::visit::visit_item(self, item);
            }
        }

        fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

        fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
            if identifier == "capture_agent_kind" {
                self.hits.push("capture_agent_kind".to_string());
            }
            if identifier == "from_db_str" || identifier == "from_cli_slug" {
                self.hits.push(format!("{identifier}("));
            }
        }

        fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
            if expr_mentions(&expr.expr, "provider_name")
                && expr.arms.iter().any(|arm| string_pattern(&arm.pat))
            {
                self.hits.push("provider_name()-keyed match".to_string());
            }
            syn::visit::visit_expr_match(self, expr);
        }

        fn visit_expr_binary(&mut self, expr: &'ast syn::ExprBinary) {
            if matches!(expr.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_))
                && ((string_literal(&expr.right) && expr_mentions(&expr.left, "provider_name"))
                    || (string_literal(&expr.left) && expr_mentions(&expr.right, "provider_name")))
            {
                self.hits
                    .push("provider_name() string comparison".to_string());
            }
            syn::visit::visit_expr_binary(self, expr);
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if mac.path.is_ident("matches")
                && mentions_identifier(mac.tokens.clone(), "provider_name")
                && has_string_literal(mac.tokens.clone())
            {
                self.hits.push("provider_name()-keyed matches!".to_string());
            }
            for identifier in ["capture_agent_kind", "from_db_str", "from_cli_slug"] {
                if mentions_identifier(mac.tokens.clone(), identifier) {
                    self.hits.push(format!("{identifier} in macro"));
                }
            }
            syn::visit::visit_macro(self, mac);
        }
    }

    let name_mapping = |source: &str| {
        let mut scan = NameMappingScan::default();
        scan.visit_file(&syn::parse_file(source).expect("parse name-mapping fixture"));
        scan.hits
    };
    let mapping_fixture = name_mapping(
        r#"
        fn capture_agent_kind(provider_name: &str) -> &str {
            match provider_name { "claude" => "claude_code", other => other }
        }
        fn parsed(value: &str) { let _ = AgentKind::from_db_str(value); }
        fn slug(value: &str) { let _ = AgentKind::from_cli_slug(value); }
        fn compared(provider: &dyn HookProvider) {
            if provider.provider_name() == "opencode" {}
            let _ = matches!(provider.provider_name(), "codex");
        }
        "#,
    );
    assert_eq!(
        mapping_fixture.len(),
        6,
        "name-mapping self-test: {mapping_fixture:?}"
    );
    let neutral_mapping_fixture = name_mapping(
        r#"
        fn neutral(provider: &dyn HookProvider, phase: &str) {
            tracing::warn!(provider = provider.provider_name(), "skipping");
            let _ = build_ai_session_id(provider.provider_name(), "session");
            match phase { "active" => (), _ => () }
        }
        #[cfg(test)]
        mod tests {
            fn legacy(name: &str) -> &str { match name { "claude" => "claude_code", other => other } }
        }
        "#,
    );
    assert!(
        neutral_mapping_fixture.is_empty(),
        "name-mapping self-test must accept provider-neutral uses: {neutral_mapping_fixture:?}"
    );
    let mut mapping_scope = rust_sources_under("src/internal/ai/hooks");
    assert!(
        mapping_scope.len() >= 10,
        "the name-mapping scan must cover hooks/**: {mapping_scope:?}"
    );
    mapping_scope.extend(
        [
            "src/internal/ai/capture/live.rs",
            "src/internal/ai/capture/live_pipeline.rs",
            "src/internal/ai/capture/live_checkpoint.rs",
        ]
        .map(String::from),
    );
    let mut mapping_violations = Vec::new();
    for relative_path in &mapping_scope {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        for hit in name_mapping(&source) {
            mapping_violations.push(format!("{relative_path}: {hit}"));
        }
    }
    assert!(
        mapping_violations.is_empty(),
        "hook and live capture layers must take the catalog kind from HookProviderIdentity, never from a provider name:\n{}",
        mapping_violations.join("\n")
    );

    // --- Provider tokens in the shared live capture modules.
    let token_scan = |source: &str| {
        let file = syn::parse_file(source).expect("parse provider-token fixture");
        let mut scan = ProviderTokenScan::default();
        scan.visit_file(&file);
        (file, scan)
    };
    let (_, positive) = token_scan(
        r#"
        fn regressed(agent_kind: &str) {
            let _ = matches!(agent_kind, "claude_code" | "codex");
            tracing::warn!("opencode export unavailable");
            if agent_kind == "opencode" {}
            let _ = AgentKind::OpenCode;
        }
        "#,
    );
    assert_eq!(
        positive.lines_outside(&[]).len(),
        4,
        "provider-token self-test (positive): {:?}",
        positive.hits
    );
    let (_, negative) = token_scan(
        r#"
        fn neutral(phase: &str, message: Message, value: Value, kind: &str, event: Event) {
            match phase { "active" => () }
            let _ = message.role == "user";
            let _ = value.get("kind").and_then(Value::as_str) == Some(kind);
            let _ = match event.kind { SubagentStart => "start", _ => "end" };
        }
        /// The OpenCode export bridge (documentation is prose).
        fn documented() {}
        #[cfg(test)]
        mod tests { fn fixture() { let _ = "claude_code"; } }
        "#,
    );
    assert!(
        negative.hits.is_empty(),
        "provider-token self-test (negative): {:?}",
        negative.hits
    );
    for relative_path in [
        "src/internal/ai/capture/live.rs",
        "src/internal/ai/capture/live_pipeline.rs",
        "src/internal/ai/capture/live_checkpoint.rs",
    ] {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        let (_, scan) = token_scan(&source);
        let lines = scan.lines_outside(&[]);
        let detail: Vec<String> = scan
            .hits
            .iter()
            .filter(|(at, _)| lines.contains(&at.line))
            .map(|(at, hit)| format!("{relative_path}:{}: {hit}", at.line))
            .collect();
        assert!(
            lines.is_empty(),
            "shared live capture must take provider decisions from LiveCaptureProvider, not provider tokens:\n{}",
            detail.join("\n")
        );
    }

    // --- Positive wiring anchors: each provider decision reaches its port.
    for (relative_path, function, required) in [
        (
            "src/internal/ai/hooks/runtime.rs",
            "effective_hook_capture_deadline",
            "effective_capture_deadline(binding.missing_host_capture_budget(), deadline)",
        ),
        (
            "src/internal/ai/capture/live_pipeline.rs",
            "ingest_agent_traces_payload_with_scope",
            "effective_capture_deadline(binding.missing_host_capture_budget(), deadline)",
        ),
        (
            "src/internal/ai/capture/live_pipeline.rs",
            "ingest_agent_traces_payload_with_scope",
            "discover_subagents(binding, &live_context, subagent_discovery_deadline)",
        ),
        (
            "src/internal/ai/capture/live_pipeline.rs",
            "discover_subagents",
            "binding.subagent_discovery()",
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            "write_committed_checkpoint",
            "acquire_live_snapshot(",
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            "acquire_live_snapshot",
            "binding.live_transcript_candidate(context)",
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            "write_committed_checkpoint",
            "binding.transcript_exporter()",
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            "run_export_stage",
            "exporter.export(context, export_deadline.monotonic())",
        ),
    ] {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {relative_path}: {error}"));
        let body = span_source_text(&source, top_level_function(&file, function).span());
        assert!(
            body.contains(required),
            "{relative_path}::{function} must route its provider decision through the live-capture binding: {required}"
        );
    }
}

/// Scope/key binding reaches canonicalize, worktree resolution, key creation,
/// and fsync. Keep managed hooks behind the same bounded subprocess pattern as
/// other potentially uninterruptible filesystem operations, without handing a
/// private key to argv, stdout, or a normal CLI parser.
#[test]
fn managed_scope_binding_uses_a_capped_killable_helper_and_absolute_deadline() {
    // ADR-ACF-10 (ACF-17): the helper, phase reader, wire protocol and scope
    // resolver live in `capture/scope_binding.rs`; only target dispatch stays
    // in the hook runtime.
    const SCOPE_BINDING: &str = "src/internal/ai/capture/scope_binding.rs";
    const RUNTIME: &str = "src/internal/ai/hooks/runtime.rs";
    let scope_binding = fs::read_to_string(repo_root().join(SCOPE_BINDING))
        .expect("read capture scope-binding source");
    let runtime = fs::read_to_string(repo_root().join(RUNTIME)).expect("read hook runtime source");
    let binding_helper = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "async fn bind_capture_scope_cwd_bounded(",
        "/// Finish the parent-side scope-proof protocol after the bounded child response",
    );
    for required in [
        "helper_program()",
        ".arg(CAPTURE_SCOPE_BINDING_HELPER_ARG)",
        ".kill_on_drop(true)",
        "configure_private_helper_process_group(&mut command)",
        "CancellationSafeChild::new_process_group",
        "CAPTURE_SCOPE_BINDING_HELPER_RESPONSE_CAP",
        "let (phase_tx, mut phase_rx)",
        "read_async_strictly_bounded(&mut stdout, CAPTURE_SCOPE_BINDING_HELPER_RESPONSE_CAP)",
        "SCOPE_BINDING_PHASE_UNVERIFIED",
        "SCOPE_BINDING_PHASE_TRUSTED",
        "tokio::time::timeout_at",
        "child.register_abort_on_cancel(&stdout_task)",
        "child.terminate_and_reap()",
    ] {
        assert!(
            binding_helper.contains(required),
            "managed scope binding must retain its bounded killable helper behavior: {required}"
        );
    }
    assert!(
        binding_helper.contains("let Some(deadline) = deadline else")
            && binding_helper.contains("return bind_capture_scope_cwd(&scope_input);"),
        "only unmanaged/no-deadline callers may retain the direct in-process binding path"
    );
    assert!(
        !binding_helper.contains("spawn_blocking"),
        "managed scope binding must not use an uncancellable Tokio blocking task"
    );
    let phase_read_at = binding_helper
        .find("let phase = match tokio::time::timeout_at")
        .expect("parent reads the helper phase before response work");
    let response_reader_at = binding_helper
        .find("let response_bytes = match")
        .expect("parent reads the trusted response after its phase");
    assert!(
        phase_read_at < response_reader_at,
        "the parent must classify the flushed phase before it can await the final response"
    );
    assert!(
        binding_helper.contains("before scope proof")
            && binding_helper.contains("after trusted scope proof"),
        "pre-proof timeout/EOF must remain advisory while post-proof failures remain trusted"
    );
    let phase_reader = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "async fn read_scope_binding_phase_until",
        "pub(crate) async fn bind_capture_scope_cwd_bounded(",
    );
    for required in [
        "tokio::time::timeout_at",
        "reader.read_exact(&mut phase)",
        "ScopeBindingPhaseRead::Byte",
        "ScopeBindingPhaseRead::EofOrIo",
        "ScopeBindingPhaseRead::TimedOut",
    ] {
        assert!(
            phase_reader.contains(required),
            "scope-proof reader must preserve bounded U/T classification: {required}"
        );
    }
    let helper_protocol = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "pub fn run_capture_scope_binding_helper_to_writer",
        "pub(crate) fn trusted_scope_binding_failure(",
    );
    for required in [
        "CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP",
        "CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP",
        "deadline_millis",
        "SCOPE_BINDING_PHASE_UNVERIFIED",
        "SCOPE_BINDING_PHASE_TRUSTED",
        "bind_verified_capture_scope",
        "write_scope_binding_phase(output, SCOPE_BINDING_PHASE_TRUSTED)",
        "write_scope_binding_response",
    ] {
        assert!(
            helper_protocol.contains(required),
            "the helper must retain a child-side deadline mutation gate: {required}"
        );
    }
    let resolve_at = helper_protocol
        .find("let verified_scope = match resolve_capture_scope_cwd")
        .expect("helper resolves scope before its success proof");
    let trusted_phase_at = helper_protocol
        .rfind("write_scope_binding_phase(output, SCOPE_BINDING_PHASE_TRUSTED)?;")
        .expect("helper flushes trusted proof before key I/O");
    let key_at = helper_protocol
        .find("bind_verified_capture_scope")
        .expect("helper crosses replay-key boundary");
    assert!(
        resolve_at < trusted_phase_at && trusted_phase_at < key_at,
        "a successful helper must prove scope before replay-key work can block"
    );

    let helper_request = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "struct ScopeBindingHelperRequest",
        "enum ScopeBindingHelperResponse",
    );
    assert!(
        helper_request.contains("event_identity_preimage")
            && helper_request.contains("dedup_preimage")
            && !helper_request.contains("provider_name:")
            && !helper_request.contains("hook_event_name:")
            && !helper_request.contains("session_id:")
            && !helper_request.contains("timestamp:")
            && !helper_request.contains("dedup_secret")
            && !helper_request.contains("native_identity"),
        "the scope helper may receive only fixed ingress digests, never raw provider/native fields or repository key material"
    );
    let helper_response = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "enum ScopeBindingHelperResponse",
        "struct ScopeBindingOpaqueDedup",
    );
    assert!(
        helper_response.contains("opaque_event")
            && helper_response.contains("opaque_dedup")
            && !helper_response.contains("dedup_secret")
            && !helper_response.contains("base64"),
        "a directly callable helper response must contain only opaque HMAC identities, never repository key material"
    );
    let helper_response_builder = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "fn scope_binding_response_from_binding",
        "fn decode_scope_binding_helper_response",
    );
    assert!(
        helper_response_builder.contains("opaque_dedup_identity_from_preimage")
            && helper_response_builder.contains("opaque_event_identity_from_preimage")
            && !helper_response_builder.contains("STANDARD.encode(dedup_secret)")
            && !helper_response_builder.contains("dedup_secret_base64"),
        "the helper must HMAC only fixed preimages in-child without serializing its key"
    );
    let ingress_path = repo_root().join("src/internal/ai/capture/ingress.rs");
    let ingress = fs::read_to_string(&ingress_path).expect("read capture ingress source");
    let opaque_wire_validator = ingress
        .split("pub(crate) fn opaque_event_id_from_wire_parts")
        .nth(1)
        .and_then(|entry| entry.split("fn opaque_commitment_event_id").next())
        .expect("opaque helper event identity validator exists");
    assert!(
        opaque_wire_validator.contains("capture-event-v1:")
            && opaque_wire_validator.contains("capture-dedup-v2:")
            && opaque_wire_validator.contains("event_id != expected"),
        "the parent must reject a helper response whose event UUID is detached from its opaque HMAC proof"
    );
    let native_preimage = ingress
        .split("fn native_dedup_preimage")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("pub(crate) fn opaque_dedup_identity_from_preimage")
                .next()
        })
        .expect("ingress-native replay preimage exists");
    for required in [
        "digest::SHA256",
        "provider.provider_name()",
        "envelope.hook_event_name",
        "envelope.session_id",
        "hash_dedup_preimage_component",
    ] {
        assert!(
            native_preimage.contains(required),
            "the ingress preimage must length-delimit every native identity component: {required}"
        );
    }
    let event_preimage = ingress
        .split("fn event_identity_preimage")
        .nth(1)
        .and_then(|entry| entry.split("fn native_dedup_preimage").next())
        .expect("ingress event-identity preimage exists");
    for required in [
        "digest::SHA256",
        "provider.provider_name()",
        "envelope.hook_event_name",
        "envelope.session_id",
        "timestamp_nanos_opt",
        "event.kind",
        "hash_dedup_preimage_component",
    ] {
        assert!(
            event_preimage.contains(required),
            "the fallback event preimage must length-delimit every total lifecycle component: {required}"
        );
    }
    let ingress_binding = ingress
        .split("pub(crate) struct CaptureIngressBinding")
        .nth(1)
        .and_then(|entry| entry.split("enum CaptureIngressDedup").next())
        .expect("capture ingress binding exists");
    assert!(
        !ingress_binding.contains("event_key")
            && !ingress_binding.contains("dedup_secret")
            && !ingress_binding.contains("session_id"),
        "the parent binding must retain only validated opaque identity/scope, never helper HMAC proof, raw identity, or repository key"
    );

    let scope_resolver = anchored_section(
        &scope_binding,
        SCOPE_BINDING,
        "fn resolve_capture_scope_cwd(",
        "/// Cross the replay-key boundary only after",
    );
    let canonical_reported_at = scope_resolver
        .find("let canonical_reported = reported_path.canonicalize()")
        .expect("reported cwd is lexically/canonically checked first");
    let active_scope_at = scope_resolver
        .find("RequestScope::try_resolve(active_workdir)")
        .expect("active worktree resolves before reported identity");
    let reported_scope_at = scope_resolver
        .rfind("RequestScope::try_resolve(canonical_reported.clone())")
        .expect("reported identity is checked after active worktree facts");
    assert!(
        canonical_reported_at < active_scope_at && active_scope_at < reported_scope_at,
        "only reported lexical/canonical failures may be unverified; active infrastructure must be classified first"
    );

    let dispatcher = anchored_section(
        &runtime,
        RUNTIME,
        "pub async fn process_hook_event_with_target(",
        "fn classify_capture_ingress_for_target(",
    );
    for required in [
        "hook_execution_deadline(deadline, terminal_target)",
        "execution_deadline,",
        "tokio::time::timeout_at",
    ] {
        assert!(
            dispatcher.contains(required),
            "managed terminal dispatch must retain one absolute settlement deadline: {required}"
        );
    }
    assert!(
        !dispatcher.contains("remaining.saturating_add(HOOK_TERMINAL_SETTLEMENT_GRACE)"),
        "terminal grace must not be re-anchored after scope binding"
    );

    let main_path = repo_root().join("src/main.rs");
    let main = fs::read_to_string(&main_path).expect("read main binary entrypoint");
    let helper_entry = main
        .split("fn run_capture_scope_binding_helper_if_requested()")
        .nth(1)
        .and_then(|entry| entry.split("/// Process entry point.").next())
        .expect("scope-binding private helper entrypoint exists");
    for required in [
        "CAPTURE_SCOPE_BINDING_HELPER_ARG",
        "CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP",
        "run_capture_scope_binding_helper_to_writer(",
    ] {
        assert!(
            helper_entry.contains(required),
            "scope-binding private helper must enforce its fixed capped protocol: {required}"
        );
    }
    let main_body = main
        .split("fn main()")
        .nth(1)
        .expect("main function exists");
    let dispatch_at = main_body
        .find("run_capture_scope_binding_helper_if_requested()")
        .expect("main dispatches the scope-binding helper");
    let tracing_at = main_body
        .find("install_broken_pipe_panic_hook();")
        .expect("main tracing setup exists");
    assert!(
        dispatch_at < tracing_at,
        "the private scope-binding helper must run before normal CLI/tracing startup"
    );
}

/// Provider session identifiers and arbitrary exporter diagnostics are ingress
/// data. They may remain in their narrow persistence/transport scopes, but
/// must not become hook telemetry fields or error chains.
#[test]
fn hook_capture_telemetry_redacts_session_ids_and_omits_exporter_stderr() {
    // ADR-ACF-10 (ACF-17/ACF-19): the live hook surface is the production
    // part of the runtime, the legacy intent writer, the shared live boundary,
    // the scope-binding helper, the live pipeline and the checkpoint writers.
    let production_of = |relative_path: &str, test_module: Option<&str>| -> String {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        match test_module {
            Some(delimiter) => source
                .split_once(delimiter)
                .unwrap_or_else(|| panic!("{relative_path}: test module `{delimiter}` is missing"))
                .0
                .to_string(),
            None => source,
        }
    };
    let intent_production = production_of(
        "src/internal/ai/hooks/intent.rs",
        Some("#[cfg(test)]\nmod tests"),
    );
    let runtime_production = production_of(
        "src/internal/ai/hooks/runtime.rs",
        Some("#[cfg(test)]\npub(crate) mod tests"),
    );
    let live_pipeline_production = production_of(
        "src/internal/ai/capture/live_pipeline.rs",
        Some("#[cfg(test)]\n#[path = \"live_pipeline_tests.rs\"]\nmod tests;"),
    );
    let live_checkpoint_production =
        production_of("src/internal/ai/capture/live_checkpoint.rs", None);
    // Positive anchors: each moved live module still carries redacted
    // session telemetry, so the scan below cannot pass by reading nothing.
    for (relative_path, production) in [
        (
            "src/internal/ai/capture/live_pipeline.rs",
            &live_pipeline_production,
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            &live_checkpoint_production,
        ),
    ] {
        assert!(
            production.contains("session_id = %redact_session_id("),
            "{relative_path}: expected redacted session telemetry in the moved live module"
        );
    }
    let live_surface = [
        runtime_production.clone(),
        intent_production.clone(),
        production_of("src/internal/ai/capture/live.rs", None),
        production_of(
            "src/internal/ai/capture/scope_binding.rs",
            Some("#[cfg(test)]\nmod tests"),
        ),
        live_pipeline_production.clone(),
        live_checkpoint_production.clone(),
    ];
    let mut session_id_fields = 0;
    for production in &live_surface {
        for line in production
            .lines()
            .filter(|line| line.contains("session_id = %"))
        {
            session_id_fields += 1;
            assert!(
                line.contains("redact_session_id("),
                "hook telemetry must redact a session identity instead of emitting it raw: {line}"
            );
        }
        assert!(
            !production.contains("error = %"),
            "hook capture telemetry must use typed safe reasons instead of rendering arbitrary error chains"
        );
    }
    assert!(
        session_id_fields > 0,
        "the live hook telemetry scan must cover at least one session_id field"
    );
    let legacy_session_recovery = anchored_section(
        &intent_production,
        "src/internal/ai/hooks/intent.rs",
        "async fn process_ai_intent_ingress(",
        "fn redact_ai_intent_string(",
    );
    assert!(
        legacy_session_recovery.contains("malformed_session_cache")
            && legacy_session_recovery.contains("continuing with a new in-memory session")
            && legacy_session_recovery.contains("corrupt_session_backup_archived")
            && legacy_session_recovery.contains("session_cache_cleanup_failed")
            && legacy_session_recovery.contains("session_history_persistence_failed")
            && legacy_session_recovery
                .contains("emit_warning(\"failed to dispatch automation hook event\")")
            && legacy_session_recovery
                .contains("emit_warning(\"failed to persist session history\")")
            && !legacy_session_recovery.contains("archive_err")
            && !legacy_session_recovery.contains("err.to_string()")
            && !legacy_session_recovery.contains("corrupt_session_backup\".to_string()")
            && !legacy_session_recovery.contains("emit_warning(format!"),
        "legacy hook recovery must retain only fixed safe diagnostics and an opaque archive marker, not error chains or session paths"
    );
    let corrupt_cache_recovery = legacy_session_recovery
        .split("Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {")
        .nth(1)
        .and_then(|entry| entry.rsplit("\n        Err(_) => {").nth(1))
        .expect("corrupt session-cache recovery branch exists");
    assert!(
        corrupt_cache_recovery.matches("eprintln!(").count() == 2
            && corrupt_cache_recovery.contains(
                "warning: failed to archive malformed session cache; continuing with a new in-memory session"
            )
            && corrupt_cache_recovery.contains(
                "warning: malformed session cache detected; recovering with a new in-memory session"
            )
            && !corrupt_cache_recovery.contains("redact_session_id")
            && !corrupt_cache_recovery.contains("format!(")
            && !corrupt_cache_recovery.contains("json!(err)")
            && !corrupt_cache_recovery.contains("err.to_string()"),
        "corrupt session-cache recovery stderr and metadata must use only fixed safe reasons"
    );

    let hooks_path = repo_root().join("src/command/hooks.rs");
    let hooks = fs::read_to_string(&hooks_path).expect("read hooks command source");
    let hooks_production = hooks
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("hooks test module delimiter exists");
    let public_error_mapper = hooks_production
        .split("pub(crate) fn map_capture_ingest_error(")
        .nth(1)
        .and_then(|entry| entry.split("#[cfg(test)]").next())
        .expect("capture error mapper exists");
    let post_ingress_error_branch = public_error_mapper
        .split("} else {")
        .nth(1)
        .expect("capture error mapper has a non-envelope branch");
    assert!(
        post_ingress_error_branch.contains(
            "capture could not be completed; retry the hook or inspect the local repository"
        ) && !post_ingress_error_branch.contains("{err}"),
        "the public hook CLI must not render post-ingress error chains that can contain session paths or provider identities"
    );

    let exporter_path = repo_root().join("src/internal/ai/observed_agents/opencode_export.rs");
    let exporter = fs::read_to_string(&exporter_path).expect("read opencode export source");
    let exporter_production = exporter
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("opencode export test module delimiter exists");
    let runner = exporter_production
        .split("async fn run_bounded_exporter(")
        .nth(1)
        .and_then(|entry| entry.split("/// Run the export under").next())
        .expect("bounded exporter runner exists");
    assert!(
        runner.contains("exporter diagnostics omitted")
            && !runner.contains("sanitized_stderr")
            && !runner.contains("stderr (redacted, capped)"),
        "arbitrary exporter stderr must not enter an error chain"
    );
    assert!(
        runner.contains("if libc::setsid() == -1")
            && runner.contains("std::io::Error::last_os_error()")
            && !runner.contains("command.process_group(0)")
            && runner.contains("deadline_at: tokio::time::Instant")
            && runner.contains("tokio::time::sleep_until(deadline_at)")
            && !runner.contains("tokio::time::sleep(limits.deadline)")
            && runner.contains(".current_dir(OPENCODE_SANDBOX_OUTER_CWD)")
            && runner.contains(".env(\"HOME\", OPENCODE_SANDBOX_HOME)")
            && runner.contains(".env(\"XDG_DATA_HOME\", OPENCODE_SANDBOX_DATA_HOME)")
            && runner.contains(".env(\"XDG_CONFIG_HOME\", OPENCODE_SANDBOX_CONFIG_HOME)"),
        "OpenCode must detach its outer Linux bwrap child from the invoking terminal without making setsid fail as a process-group leader"
    );
    let sandboxed_export = exporter_production
        .split("pub async fn run_export_subprocess_sandboxed(")
        .nth(1)
        .and_then(|entry| entry.split("/// Assembled Required-sandbox argv").next())
        .expect("sandboxed OpenCode export entrypoint exists");
    let macos_export = sandboxed_export
        .split("#[cfg(target_os = \"macos\")]")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("#[cfg(not(any(target_os = \"linux\", target_os = \"macos\")))]")
                .next()
        })
        .expect("macOS OpenCode export branch exists");
    assert!(
        macos_export.contains("unsupported on macOS")
            && macos_export.contains("fail-closed")
            && !macos_export.contains("assemble_sandboxed_export")
            && !macos_export.contains("run_bounded_exporter"),
        "macOS OpenCode export must fail closed before assembling or spawning an exporter"
    );
    let sandbox_runtime_path = repo_root().join("src/internal/ai/sandbox/runtime.rs");
    let sandbox_runtime =
        fs::read_to_string(&sandbox_runtime_path).expect("read bwrap sandbox source");
    let bwrap_args = sandbox_runtime
        .split("pub fn create_bwrap_command_args_with_seccomp(")
        .nth(1)
        .and_then(|entry| entry.split("if let Some(fd) = seccomp_fd").next())
        .expect("bwrap argument builder exists");
    assert!(
        bwrap_args.contains("--unshare-all")
            && bwrap_args.contains("--die-with-parent")
            && bwrap_args.contains("--new-session"),
        "the shared bwrap profile must retain its generic session-isolation contract"
    );
    let opencode_assembly = exporter_production
        .split("fn assemble_sandboxed_export(")
        .nth(1)
        .and_then(|entry| entry.split("fn remove_opencode_bwrap_new_session(").next())
        .expect("OpenCode-specific bwrap assembly exists");
    assert!(
        opencode_assembly.contains("writable_fd_binds")
            && opencode_assembly.contains("OPENCODE_SANDBOX_OUTER_CWD")
            && opencode_assembly.contains("OPENCODE_SANDBOX_EXPORTER")
            && opencode_assembly.contains("extra_ro_bind_paths: &[]")
            && opencode_assembly.contains("insert_opencode_private_mounts")
            && opencode_assembly.contains("replace_opencode_path_fd_binds")
            && !opencode_assembly.contains("binary.parent()"),
        "OpenCode must mount a sealed exporter capability at a fixed private target, never ambient HOME/XDG or the exporter parent"
    );
    let opencode_session_adjustment = exporter_production
        .split("fn remove_opencode_bwrap_new_session(")
        .nth(1)
        .and_then(|entry| entry.split("fn which_bwrap()").next())
        .expect("OpenCode-specific session adjustment exists");
    assert!(
        opencode_session_adjustment.contains("let [separator] = separators.as_slice()")
            && opencode_session_adjustment.contains("OPENCODE_SANDBOX_EXPORTER")
            && opencode_session_adjustment.contains("expected_exporter_fd")
            && opencode_session_adjustment.contains("\"--unshare-net\"")
            && opencode_session_adjustment.contains("\"--share-net\"")
            && opencode_session_adjustment.contains("\"--ro-bind\"")
            && opencode_session_adjustment.contains("\"--ro-bind-fd\"")
            && opencode_session_adjustment.contains("\"--bind\"")
            && opencode_session_adjustment.contains("\"--bind-fd\"")
            && opencode_session_adjustment.contains("/proc/self/fd/")
            && opencode_session_adjustment.contains("retained a path-string descriptor mount")
            && opencode_session_adjustment.contains("exporter_fd_bind")
            && opencode_session_adjustment.contains("remaining_ro_bind_paths")
            && opencode_session_adjustment.contains("remaining_writable_binds")
            && opencode_session_adjustment.contains("command.remove(new_session_index)"),
        "OpenCode must accept only its canonical Required bwrap grammar, including exactly one sealed RO exporter FD bind and its private writable store bind, before removing exactly one --new-session"
    );
    let sealed_exporter = exporter_production
        .split("fn pin_exporter_fd(")
        .nth(1)
        .and_then(|entry| entry.split("fn which_bwrap()").next())
        .expect("sealed OpenCode exporter fd helper exists");
    assert!(
        sealed_exporter.contains("metadata.dev() != expected.device")
            && sealed_exporter.contains("metadata.ino() != expected.inode")
            && sealed_exporter.contains("metadata.mtime() != expected.mtime")
            && sealed_exporter.contains("Sha256::new()")
            && sealed_exporter.contains("tempfile::tempfile()")
            && sealed_exporter.contains(".write_all(&buffer")
            && sealed_exporter.contains("set_permissions")
            && sealed_exporter.contains("libc::O_CLOEXEC")
            && sealed_exporter.contains("F_DUPFD_CLOEXEC"),
        "OpenCode must verify trust provenance then execute a private sealed copy, not a mutable trusted-directory fd"
    );
    let bwrap_probe = exporter_production
        .split("async fn trusted_bwrap_supports_fd_mounts_until(")
        .nth(1)
        .and_then(|entry| entry.split("/// Non-Linux hosts").next())
        .expect("async descriptor-native bwrap capability probe exists");
    assert!(
        bwrap_probe.contains("new_bwrap_fd_mount_probe_until(deadline).await")
            && bwrap_probe.contains("\"--bind-fd\"")
            && bwrap_probe.contains("\"--ro-bind-fd\"")
            && bwrap_probe.contains("prepare_bwrap_capability_fds_for_exec")
            && bwrap_probe.contains("tokio::time::timeout_at(deadline")
            && bwrap_probe.contains("ExporterCancellationGuard")
            && bwrap_probe.contains(".env_clear()"),
        "OpenCode bwrap availability must use the same bounded async descriptor-FD probe as production, including RW store and sealed RO exporter mounts"
    );
    let descriptor_probe_setup = exporter_production
        .split("fn new_bwrap_fd_mount_probe()")
        .nth(1)
        .and_then(|entry| entry.split("fn bwrap_fd_mount_probe_succeeded").next())
        .expect("descriptor-FD probe setup exists");
    assert!(
        descriptor_probe_setup.contains("pin_store_under")
            && descriptor_probe_setup.contains("pin_exporter_fd")
            && descriptor_probe_setup.contains("host-only-sibling")
            && descriptor_probe_setup.contains("libra-sealed-exporter-probe")
            && descriptor_probe_setup.contains("/proc/self/fd/[0-9]*")
            && descriptor_probe_setup.contains("printf mutated"),
        "bwrap probe must prove both descriptor mount forms work while no payload fd can traverse the host parent or mutate the sealed exporter"
    );
    let deadline_export = exporter_production
        .split("pub async fn authorized_trusted_sandboxed_export_until(")
        .nth(1)
        .and_then(|entry| entry.split("/// Bind an OpenCode export").next())
        .expect("deadline-aware trusted OpenCode export entrypoint exists");
    assert!(
        deadline_export.contains("run_trusted_export_subprocess_sandboxed_until")
            && deadline_export.contains("deadline"),
        "hook capture must enter OpenCode export through the absolute-deadline-aware trusted path"
    );
    // ADR-ACF-10 (ACF-20): the bridge call lives in the OpenCode exporter
    // capability; the export stage hands it the capture deadline.
    let live_capture_exporter = production_of(
        "src/internal/ai/observed_agents/live_capture.rs",
        Some("#[cfg(test)]\n#[cfg_attr(not(unix), allow(dead_code))]\npub(crate) mod test_support"),
    );
    assert!(
        sandboxed_export.contains("pin_revalidated_opencode_exporter_until(binary, deadline)")
            && sandboxed_export.contains("run_export_subprocess_sandboxed_with_exporter_fd(")
            && sandboxed_export.contains("limits, deadline")
            && live_capture_exporter.contains("authorized_trusted_sandboxed_export_until(")
            && live_checkpoint_production.contains("let export_deadline = capture_deadline")
            && live_checkpoint_production
                .contains("exporter.export(context, export_deadline.monotonic())"),
        "OpenCode capture must carry the hook's original absolute deadline through trust sealing, bwrap probing, and the bounded runner"
    );
    let trust_path = repo_root().join("src/internal/ai/observed_agents/trust.rs");
    let trust = fs::read_to_string(&trust_path).expect("read external-agent trust source");
    let trust_revalidation = trust
        .split("pub async fn revalidate_trust(")
        .nth(1)
        .and_then(|entry| entry.split("#[cfg(test)]").next())
        .expect("trust revalidation exists");
    assert!(
        trust_revalidation.contains("tokio::task::spawn_blocking")
            && trust_revalidation.contains("compute_provenance"),
        "trusted exporter provenance hashing must not block Tokio's capture executor"
    );

    let transcript_source_path =
        repo_root().join("src/internal/ai/observed_agents/transcript_source.rs");
    let transcript_source =
        fs::read_to_string(&transcript_source_path).expect("read transcript source resolver");
    let transcript_source_production = transcript_source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("transcript source test module delimiter exists");
    let source_resolver = transcript_source_production
        .split("fn resolve_transcript_source_with_policy(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("/// Resolve a source for existing live hook capture.")
                .next()
        })
        .expect("transcript source resolver exists");
    assert!(
        source_resolver.contains("reason = \"transcript_preparer_failed\"")
            && !source_resolver.contains("error = %")
            && !source_resolver.contains("format!(\"{err:#}\")"),
        "transcript preparer telemetry must use a fixed reason, not source-derived error text"
    );

    let claude_path = repo_root().join("src/internal/ai/observed_agents/builtin/claude_code.rs");
    let claude = fs::read_to_string(&claude_path).expect("read Claude adapter source");
    let claude_production = claude
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("Claude adapter test module delimiter exists");
    let claude_preparer = claude_production
        .split("impl TranscriptPreparer for ClaudeCodeObservedAgent {")
        .nth(1)
        .and_then(|entry| entry.split("// ---------------------------------------------------------------------------").next())
        .expect("Claude transcript preparer exists");
    assert!(
        claude_preparer.contains("reason = \"flush_wait_budget_exhausted\"")
            && !claude_preparer.contains("session_id = %")
            && !claude_preparer.contains("session.session_id"),
        "Claude preparer telemetry must not emit any native session identity"
    );

    let coordinator_path = repo_root().join("src/internal/ai/capture/coordinator.rs");
    let coordinator =
        fs::read_to_string(&coordinator_path).expect("read capture coordinator source");
    let coordinator_production = coordinator
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("capture coordinator test module delimiter exists");
    let pending_cleanup = coordinator_production
        .split("CheckpointWriteOutcome::PendingCleanup { checkpoint_id, .. } => {")
        .nth(1)
        .and_then(|entry| entry.split("CheckpointWriteOutcome::Written {").next())
        .expect("pending-cleanup finalizer branch exists");
    assert!(
        pending_cleanup.contains("finalizer_reason = \"finalizer_update_failed\"")
            && !pending_cleanup.contains("error = ?")
            && !pending_cleanup.contains("error = %"),
        "pending-cleanup finalizer telemetry must not render an arbitrary error chain"
    );

    let subagent_path = repo_root().join("src/internal/ai/subagent_content.rs");
    let subagent = fs::read_to_string(&subagent_path).expect("read subagent content source");
    let subagent_production = subagent
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("subagent test module delimiter exists");
    let committed_marker_cleanup = subagent_production
        .split("failed to clear committed subagent content in-flight marker")
        .next()
        .and_then(|entry| entry.rsplit("tracing::warn!(").next())
        .expect("committed subagent marker cleanup warning exists");
    assert!(
        committed_marker_cleanup
            .contains("cleanup_reason = \"clear_committed_subagent_marker_failed\"")
            && !committed_marker_cleanup.contains("error = %")
            && !committed_marker_cleanup.contains("format!(\"{error:#}\")"),
        "subagent marker-cleanup telemetry must retain only a fixed safe reason"
    );

    let history_path = repo_root().join("src/internal/ai/history.rs");
    let history = fs::read_to_string(&history_path).expect("read history source");
    let rejected_cleanup = history
        .split("async fn cleanup_rejected_checkpoint_objects(")
        .nth(1)
        .and_then(|entry| {
            entry
                .split("async fn claim_rejected_checkpoint_cleanup_job(")
                .next()
        })
        .expect("rejected checkpoint cleanup helper exists");
    assert!(
        !rejected_cleanup.contains("session_id = %writer_fence.session_id"),
        "rejected checkpoint cleanup telemetry must not emit a raw capture session identity"
    );
}

/// ACF-03 keeps every provider-native transcript read at the snapshot
/// boundary. The hook runtime may select a provider-derived context, but it
/// must not resolve a source, read a held descriptor, or run the source
/// redactor itself: those operations would let live/import semantics drift.
#[test]
fn transcript_reads_only_through_snapshot_service() {
    // ADR-ACF-10 (ACF-19): the checkpoint writers that call the snapshot
    // service live in `capture/live_checkpoint.rs`; the forbidden-read scan
    // covers every live hook module's production text.
    let production_of = |relative_path: &str, test_module: Option<&str>| -> String {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        match test_module {
            Some(delimiter) => source
                .split_once(delimiter)
                .unwrap_or_else(|| panic!("{relative_path}: test module `{delimiter}` is missing"))
                .0
                .to_string(),
            None => source,
        }
    };
    let live_checkpoint = production_of("src/internal/ai/capture/live_checkpoint.rs", None);
    for service_call in [
        "CaptureSnapshotService::capture_live_until(",
        "CaptureSnapshotService::capture_authorized(",
    ] {
        assert!(
            live_checkpoint.contains(service_call),
            "checkpoint writers must route transcript work through snapshot service: {service_call}"
        );
    }
    let live_modules = [
        (
            "src/internal/ai/hooks/runtime.rs",
            Some("#[cfg(test)]\npub(crate) mod tests"),
        ),
        (
            "src/internal/ai/hooks/intent.rs",
            Some("#[cfg(test)]\nmod tests"),
        ),
        (
            "src/internal/ai/capture/scope_binding.rs",
            Some("#[cfg(test)]\nmod tests"),
        ),
        ("src/internal/ai/capture/live.rs", None),
        (
            "src/internal/ai/capture/live_pipeline.rs",
            Some("#[cfg(test)]\n#[path = \"live_pipeline_tests.rs\"]\nmod tests;"),
        ),
        ("src/internal/ai/capture/live_checkpoint.rs", None),
    ];
    for (relative_path, test_module) in live_modules {
        let production = production_of(relative_path, test_module);
        for forbidden in [
            "resolve_transcript_source(",
            "resolve_live_transcript_source_until(",
            "resolve_import_transcript_source(",
            "read_transcript(",
            "read_live_claude_source_until(",
            ".read_bounded(",
            ".read_bounded_counted(",
            ".into_rewound_inner(",
        ] {
            assert!(
                !production.contains(forbidden),
                "{relative_path} must not bypass capture::snapshot via {forbidden}"
            );
        }
    }

    let authorized_read_path = repo_root().join("src/internal/ai/authorized_read.rs");
    let authorized_read = fs::read_to_string(&authorized_read_path)
        .expect("read authorized descriptor helper source");
    let live_helper = authorized_read
        .split("pub(crate) async fn read_live_claude_source_until(")
        .nth(1)
        .and_then(|entry| entry.split("/// Run a registered helper").next())
        .expect("live Claude descriptor helper exists");
    assert!(
        live_helper.contains(".env_clear()")
            && live_helper.matches(".current_dir(").count() == 1
            && live_helper.contains(".current_dir(std::path::Path::new(\"/\"))"),
        "{} live descriptor helper must not inherit caller environment or working directory",
        authorized_read_path.display()
    );
}

/// ACF-02: lifecycle reduction must remain a deterministic, provider-neutral
/// function.  A provider adapter may choose a canonical `LifecycleEventKind`,
/// but it must never leak its identity or an I/O capability into the reducer.
/// The guard is deliberately dependency-based: it permits the normalized hook
/// event contract while rejecting persistence, time, filesystem, ref, and
/// provider-adapter imports even when an alias is used.
#[test]
fn capture_reducer_is_pure_and_provider_neutral() {
    use syn::visit::Visit;

    fn forbidden_reason(path: &str) -> Option<&'static str> {
        let path = path.strip_prefix("crate::").unwrap_or(path);
        let starts = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}::"));
        if starts("internal::ai::observed_agents")
            || starts("ai::observed_agents")
            || starts("internal::ai::hooks::providers")
            || starts("ai::hooks::providers")
            || matches!(path, "AgentKind" | "ObservedAgent" | "HookProvider")
        {
            return Some("provider identity/adapter dependency");
        }
        if starts("sea_orm")
            || starts("internal::db")
            || starts("db")
            || starts("internal::ai::capture_scope")
            || starts("ai::capture_scope")
            || matches!(
                path,
                "DatabaseConnection" | "DatabaseTransaction" | "CaptureScope"
            )
        {
            return Some("database or capture-scope dependency");
        }
        if starts("internal::ai::capture::catalog")
            || starts("ai::capture::catalog")
            || starts("internal::ai::capture::checkpoint")
            || starts("ai::capture::checkpoint")
            || starts("internal::ai::history")
            || starts("ai::history")
            || starts("internal::ai::traces")
            || starts("ai::traces")
            || matches!(
                path,
                "CaptureCatalogPort" | "CheckpointStore" | "HistoryManager" | "CheckpointCommit"
            )
        {
            return Some("catalog, checkpoint, history, or ref dependency");
        }
        if starts("std::fs")
            || starts("std::path")
            || starts("std::env")
            || starts("std::process")
            || starts("std::net")
            || starts("tokio::fs")
            || starts("chrono")
            || starts("std::time")
            || starts("tokio::time")
            || matches!(path, "Utc" | "Instant" | "SystemTime")
        {
            return Some("clock, filesystem, process, or network dependency");
        }
        None
    }

    struct ReducerBoundaryGuard {
        violations: Vec<String>,
        reducer_count: usize,
    }

    impl<'ast> Visit<'ast> for ReducerBoundaryGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = forbidden_reason(&path) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
                if path
                    .strip_prefix("crate::")
                    .is_some_and(|path| path == "internal::ai::hooks")
                {
                    self.violations.push(
                        "use crate::internal::ai::hooks → module alias can bypass the normalized-event allowlist"
                            .to_string(),
                    );
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let rendered = syn_path_text(path);
            if let Some(reason) = forbidden_reason(&rendered) {
                self.violations.push(format!("path {rendered} → {reason}"));
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            if function.sig.ident == "reduce_lifecycle" {
                self.reducer_count += 1;
                if function.sig.asyncness.is_some() {
                    self.violations.push(
                        "reduce_lifecycle is async; a reducer must not await I/O".to_string(),
                    );
                }
                if function.sig.inputs.len() != 1 {
                    self.violations.push(format!(
                        "reduce_lifecycle accepts {} inputs; its complete state/event/deadline input must stay typed and singular",
                        function.sig.inputs.len()
                    ));
                }
            }
            syn::visit::visit_item_fn(self, function);
        }

        fn visit_expr_unsafe(&mut self, expression: &'ast syn::ExprUnsafe) {
            self.violations.push(
                "unsafe code in capture/state.rs would defeat the reducer's pure-data boundary"
                    .to_string(),
            );
            syn::visit::visit_expr_unsafe(self, expression);
        }
    }

    let (path, file) = parse_rust_source("src/internal/ai/capture/state.rs");
    let mut guard = ReducerBoundaryGuard {
        violations: Vec::new(),
        reducer_count: 0,
    };
    guard.visit_file(&file);
    assert_eq!(
        guard.reducer_count,
        1,
        "{} must expose exactly one reduce_lifecycle entrypoint",
        path.display()
    );
    assert!(
        guard.violations.is_empty(),
        "capture reducer must stay pure and provider-neutral:\n{}",
        guard.violations.join("\n")
    );
}

/// ACF-04 makes `CaptureCatalogStore` the sole writer for the durable
/// `agent_session` / `agent_checkpoint` catalog. ADR-ACF-10 (ACF-19) moves the
/// remaining hook-side read probes into `capture/live.rs`: the hook entry, the
/// legacy intent writer, the scope-binding helper, the live pipeline and the
/// checkpoint writers hold no SQL-shaped literal at all, and `capture/live.rs`
/// holds only the narrowly-scoped SELECT probes. A hand-written mutation in
/// any of them would reintroduce a second write protocol outside the
/// fence/replay receipt transaction.
#[test]
fn hook_runtime_has_no_catalog_mutation_sql() {
    use syn::visit::Visit;

    /// ADR-ACF-10 SQL-shaped literal: the first token is an uppercase
    /// `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`WITH`/`PRAGMA` followed by
    /// whitespace (case-sensitive, so prose such as `Delete …` is not SQL).
    fn sql_shaped(value: &str) -> Option<&'static str> {
        let value = value.trim_start();
        ["SELECT", "INSERT", "UPDATE", "DELETE", "WITH", "PRAGMA"]
            .into_iter()
            .find(|keyword| {
                value
                    .strip_prefix(keyword)
                    .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            })
    }

    #[derive(Default)]
    struct SqlLiteralGuard {
        sql_literals: Vec<(&'static str, String)>,
        mutations: Vec<String>,
        capture_reads: Vec<String>,
    }

    impl SqlLiteralGuard {
        fn literal(&mut self, value: &str) {
            let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(table) = sql_capture_catalog_mutation(value) {
                self.mutations
                    .push(format!("{table} mutation in SQL literal {normalized:?}"));
            }
            if let Some(keyword) = sql_shaped(value) {
                self.sql_literals.push((keyword, normalized));
            }
            let words = source_words(value);
            if words.first().is_some_and(|word| word == "SELECT") {
                for window in words.windows(2) {
                    if matches!(window[0].as_str(), "FROM" | "JOIN")
                        && is_capture_catalog_table(&window[1])
                    {
                        self.capture_reads.push(window[1].to_ascii_lowercase());
                    }
                }
            }
        }

        /// Macro bodies are token trees that syn does not parse as
        /// expressions; walk them so `format!("UPDATE …")` is still seen.
        fn macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            for token in tokens {
                match token {
                    proc_macro2::TokenTree::Group(group) => self.macro_tokens(group.stream()),
                    proc_macro2::TokenTree::Literal(literal) => {
                        if let Ok(literal) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                            self.literal(&literal.value());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for SqlLiteralGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        // Attribute and doc text is prose, not an executable statement.
        fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

        fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
            self.literal(&literal.value());
            syn::visit::visit_lit_str(self, literal);
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.macro_tokens(mac.tokens.clone());
            syn::visit::visit_macro(self, mac);
        }
    }

    // Non-vacuity: SQL-shaped literals are found in expressions and macro
    // token trees; prose, a non-keyword prefix, docs and cfg(test) are not.
    let fixture = syn::parse_file(
        r#"
        fn regressed() {
            let _ = "SELECT 1 FROM agent_session";
            let _ = format!("UPDATE agent_session SET state = {}", 1);
            let _ = "Delete the stale marker";
            let _ = "WITHOUT ROWID is not a statement";
        }
        /// SELECT 1 FROM agent_session in documentation is prose.
        fn documented() {}
        #[cfg(test)]
        mod tests {
            fn allowed() {
                let _ = "INSERT INTO agent_session VALUES (1)";
            }
        }
        "#,
    )
    .expect("parse SQL-literal guard fixture");
    let mut fixture_guard = SqlLiteralGuard::default();
    fixture_guard.visit_file(&fixture);
    assert_eq!(
        fixture_guard
            .sql_literals
            .iter()
            .map(|(keyword, _)| *keyword)
            .collect::<Vec<_>>(),
        ["SELECT", "UPDATE"],
        "SQL-literal guard self-test: {:?}",
        fixture_guard.sql_literals
    );
    assert_eq!(
        fixture_guard.mutations.len(),
        1,
        "SQL-literal guard self-test must see the macro-token catalog mutation: {:?}",
        fixture_guard.mutations
    );

    // ADR-ACF-10 ACF-19 AC4: these files hold no SQL-shaped literal at all.
    for relative_path in [
        "src/internal/ai/hooks/runtime.rs",
        "src/internal/ai/hooks/intent.rs",
        "src/internal/ai/capture/scope_binding.rs",
        "src/internal/ai/capture/live_pipeline.rs",
        "src/internal/ai/capture/live_checkpoint.rs",
    ] {
        let (path, file) = parse_rust_source(relative_path);
        let mut guard = SqlLiteralGuard::default();
        guard.visit_file(&file);
        assert!(
            guard.sql_literals.is_empty() && guard.mutations.is_empty(),
            "{} must not hold SQL-shaped literals; read through the capture::live probes and write through the capture catalog/checkpoint ports:\n{:?}\n{}",
            path.display(),
            guard.sql_literals,
            guard.mutations.join("\n")
        );
    }

    // `capture/live.rs` owns the read-only probe inventory: no DML-shaped
    // literal and no catalog mutation.
    let (live_path, live_file) = parse_rust_source("src/internal/ai/capture/live.rs");
    let mut live = SqlLiteralGuard::default();
    live.visit_file(&live_file);
    let dml: Vec<&(&str, String)> = live
        .sql_literals
        .iter()
        .filter(|(keyword, _)| matches!(*keyword, "INSERT" | "UPDATE" | "DELETE"))
        .collect();
    assert!(
        dml.is_empty() && live.mutations.is_empty(),
        "{} must hold only read-only probes; route mutations through capture catalog/checkpoint ports:\n{dml:?}\n{}",
        live_path.display(),
        live.mutations.join("\n")
    );

    // This is an intentionally small, explicit read-only inventory.  Adding
    // another table to the live capture SQL is a layering change and
    // requires a corresponding review rather than silently becoming a new
    // side channel around the catalog.
    for table in &live.capture_reads {
        assert!(
            matches!(table.as_str(), "agent_session" | "agent_checkpoint"),
            "live capture read escaped the explicit catalog read inventory: {table}"
        );
    }
    for table in ["agent_session", "agent_checkpoint"] {
        assert!(
            live.capture_reads.iter().any(|read| read == table),
            "{} must retain its scoped {table} read probes rather than rebuilding state from an unfenced source",
            live_path.display()
        );
    }
}

/// ACF-05 centralizes traces-history append and the writer-marker lifecycle
/// in `TracesCheckpointStore`.  The runtime still serves the unrelated
/// AI-intent history flow, so this guard is deliberately narrow: it rejects
/// the checkpoint-specific manager, the append operation, and the traces
/// marker registration/refresh/cleanup/prune entry points rather than banning
/// every use of the generic history namespace.
#[test]
fn hook_runtime_has_no_direct_history_manager_append() {
    use syn::visit::Visit;

    /// Final path segment or method name of an operation the checkpoint
    /// store owns. Prefixes cover every `_with_capture_scope` / `_until`
    /// variant of the same marker operation.
    fn store_owned_operation(name: &str) -> Option<&'static str> {
        const PREFIXES: &[(&str, &str)] = &[
            ("register_traces_write_attempt", "marker registration"),
            ("update_traces_inflight_marker", "marker refresh"),
            ("clear_traces_inflight_marker", "marker cleanup"),
            ("clear_non_cleanup_traces_inflight_marker", "marker cleanup"),
        ];
        const EXACT: &[(&str, &str)] = &[
            ("append_checkpoint_commit", "checkpoint append operation"),
            ("write_traces_inflight_marker", "marker registration"),
            ("retire_stale_traces_inflight_marker", "marker cleanup"),
            ("repair_expired_traces_inflight_marker", "marker cleanup"),
            ("prune_checkpoint_commits", "checkpoint prune"),
        ];
        PREFIXES
            .iter()
            .find(|(prefix, _)| name.starts_with(prefix))
            .or_else(|| EXACT.iter().find(|(exact, _)| name == *exact))
            .map(|(_, kind)| *kind)
    }

    fn last_segment(path: &str) -> &str {
        path.rsplit("::").next().unwrap_or(path)
    }

    struct HistoryAppendGuard {
        violations: Vec<String>,
    }

    impl<'ast> Visit<'ast> for HistoryAppendGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                if path.ends_with("::HistoryManager") || path == "HistoryManager" {
                    self.violations
                        .push(format!("imports checkpoint writer {path}"));
                }
                if let Some(kind) = store_owned_operation(last_segment(&path)) {
                    self.violations.push(format!("imports {kind} {path}"));
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let rendered = syn_path_text(path);
            if rendered.ends_with("::HistoryManager") || rendered == "HistoryManager" {
                self.violations
                    .push(format!("uses checkpoint writer {rendered}"));
            }
            if let Some(kind) = store_owned_operation(last_segment(&rendered)) {
                self.violations.push(format!("uses {kind} {rendered}"));
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let method = call.method.to_string();
            if let Some(kind) = store_owned_operation(&method) {
                self.violations
                    .push(format!("calls {kind} {method} directly"));
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    fn violations(file: &syn::File) -> Vec<String> {
        let mut guard = HistoryAppendGuard {
            violations: Vec::new(),
        };
        guard.visit_file(file);
        guard.violations
    }

    // Non-vacuity: every banned spelling in production code is reported,
    // while the same calls inside a `#[cfg(test)]` module are allowed.
    let fixture = syn::parse_file(
        r#"
        use crate::internal::ai::traces::register_traces_write_attempt_with_capture_scope_until;
        async fn regressed(conn: &C, manager: &M, marker: &T) {
            traces::write_traces_inflight_marker(conn, marker).await;
            traces::update_traces_inflight_marker_if_generation(conn, marker, "g").await;
            traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope(conn).await;
            traces::clear_traces_inflight_marker_if_generation(conn).await;
            traces::retire_stale_traces_inflight_marker(conn).await;
            manager.append_checkpoint_commit(params).await;
            manager.prune_checkpoint_commits(plan).await;
            manager.repair_expired_traces_inflight_marker(a, b, 0).await;
        }
        #[cfg(test)]
        mod tests {
            async fn allowed(manager: &M) {
                manager.prune_checkpoint_commits(plan).await;
                traces::write_traces_inflight_marker(conn, marker).await;
            }
        }
        "#,
    )
    .expect("parse checkpoint-store guard fixture");
    let fixture_violations = violations(&fixture);
    for banned in [
        "register_traces_write_attempt_with_capture_scope_until",
        "write_traces_inflight_marker",
        "update_traces_inflight_marker_if_generation",
        "clear_non_cleanup_traces_inflight_marker_with_capture_scope",
        "clear_traces_inflight_marker_if_generation",
        "retire_stale_traces_inflight_marker",
        "append_checkpoint_commit",
        "prune_checkpoint_commits",
        "repair_expired_traces_inflight_marker",
    ] {
        assert!(
            fixture_violations
                .iter()
                .any(|violation| violation.ends_with(banned)
                    || violation.ends_with(&format!("{banned} directly"))),
            "guard must reject production use of {banned}: {fixture_violations:#?}"
        );
    }
    assert_eq!(
        fixture_violations.len(),
        9,
        "cfg(test) uses must stay allowed and each banned use reported once: {fixture_violations:#?}"
    );

    // ADR-ACF-10 (ACF-17/ACF-19): the runtime, the legacy intent writer and
    // every capture live module; each anchor proves the file is the real one.
    for (relative_path, anchor) in [
        (
            "src/internal/ai/hooks/runtime.rs",
            "process_hook_event_with_target",
        ),
        (
            "src/internal/ai/hooks/intent.rs",
            "process_ai_intent_ingress",
        ),
        ("src/internal/ai/capture/live.rs", "new_ingest_span"),
        (
            "src/internal/ai/capture/scope_binding.rs",
            "bind_capture_scope_cwd_bounded",
        ),
        (
            "src/internal/ai/capture/live_pipeline.rs",
            "ingest_agent_traces_payload_with_scope",
        ),
        (
            "src/internal/ai/capture/live_checkpoint.rs",
            "write_committed_checkpoint",
        ),
    ] {
        let (path, file) = parse_rust_source(relative_path);
        top_level_function(&file, anchor);
        let file_violations = violations(&file);
        assert!(
            file_violations.is_empty(),
            "{} must delegate checkpoint history append and marker lifecycle to TracesCheckpointStore:\n{}",
            path.display(),
            file_violations.join("\n")
        );
    }
}

/// ACF-06 freezes the intended orchestration seam.  The coordinator is
/// generic over catalog/checkpoint ports and cannot observe a provider, while
/// the runtime's validated capture handoff delegates effect ordering to that
/// coordinator instead of manually calling catalog/checkpoint methods.
///
/// ACF-20 (ADR-ACF-10) completes original AC6: the hook runtime only frames
/// IO, resolves the provider once, hands off and maps output — its exact
/// non-test inventory, its production line count (≤480) and its dependency
/// ban are pinned here, the handoff is judged over the pipeline and all four
/// checkpoint-writer functions, and only `ExportJobLeaseStore` names
/// `export_job`.
#[test]
fn coordinator_is_provider_neutral_and_runtime_is_thin() {
    use syn::visit::Visit;

    fn coordinator_forbidden_reason(path: &str) -> Option<&'static str> {
        let path = path.strip_prefix("crate::").unwrap_or(path);
        let starts = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}::"));
        if starts("internal::ai::observed_agents")
            || starts("ai::observed_agents")
            || starts("internal::ai::hooks")
            || starts("ai::hooks")
            || matches!(
                path,
                "AgentKind"
                    | "ObservedAgent"
                    | "HookProvider"
                    | "LifecycleEvent"
                    | "LifecycleEventKind"
            )
        {
            return Some("provider or hook-event dependency");
        }
        if matches!(
            path,
            "CaptureCatalogStore"
                | "TracesCheckpointStore"
                | "DatabaseConnection"
                | "DatabaseTransaction"
                | "HistoryManager"
        ) || path.ends_with("::CaptureCatalogStore")
            || path.ends_with("::TracesCheckpointStore")
            || path.ends_with("::DatabaseConnection")
            || path.ends_with("::DatabaseTransaction")
            || path.ends_with("::HistoryManager")
        {
            return Some("concrete persistence implementation dependency");
        }
        None
    }

    struct CoordinatorBoundaryGuard {
        violations: Vec<String>,
        port_traits_seen: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for CoordinatorBoundaryGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = coordinator_forbidden_reason(&path) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
                if path.ends_with("::CaptureCatalogPort") || path == "CaptureCatalogPort" {
                    self.port_traits_seen.insert("catalog".to_string());
                }
                if path.ends_with("::CheckpointStore") || path == "CheckpointStore" {
                    self.port_traits_seen.insert("checkpoint".to_string());
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let rendered = syn_path_text(path);
            if let Some(reason) = coordinator_forbidden_reason(&rendered) {
                self.violations.push(format!("path {rendered} → {reason}"));
            }
            if rendered.ends_with("::CaptureCatalogPort") || rendered == "CaptureCatalogPort" {
                self.port_traits_seen.insert("catalog".to_string());
            }
            if rendered.ends_with("::CheckpointStore") || rendered == "CheckpointStore" {
                self.port_traits_seen.insert("checkpoint".to_string());
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
            for word in source_words(&literal.value()) {
                if matches!(
                    word.as_str(),
                    "CLAUDE" | "CODEX" | "OPENCODE" | "GEMINI" | "PI"
                ) {
                    self.violations.push(format!(
                        "provider-specific literal {:?} in coordinator",
                        literal.value()
                    ));
                    break;
                }
            }
            syn::visit::visit_lit_str(self, literal);
        }
    }

    let (coordinator_path, coordinator_file) =
        parse_rust_source("src/internal/ai/capture/coordinator.rs");
    let mut coordinator_guard = CoordinatorBoundaryGuard {
        violations: Vec::new(),
        port_traits_seen: BTreeSet::new(),
    };
    coordinator_guard.visit_file(&coordinator_file);
    assert!(
        coordinator_guard.violations.is_empty(),
        "{} must not depend on providers, hook event taxonomy, or concrete stores:\n{}",
        coordinator_path.display(),
        coordinator_guard.violations.join("\n")
    );
    assert_eq!(
        coordinator_guard.port_traits_seen,
        BTreeSet::from(["catalog".to_string(), "checkpoint".to_string()]),
        "coordinator must be wired only through both catalog and checkpoint port traits"
    );

    fn receiver_name(expression: &syn::Expr) -> Option<String> {
        match expression {
            syn::Expr::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            syn::Expr::Reference(reference) => receiver_name(&reference.expr),
            syn::Expr::Paren(paren) => receiver_name(&paren.expr),
            _ => None,
        }
    }

    struct RuntimeHandoffGuard {
        coordinator_type_seen: bool,
        coordinator_execution_calls: usize,
        preapplied_execution_calls: usize,
        coordinator_reservations: usize,
        deadline_aware_coordinator_reservations: usize,
        direct_effect_calls: Vec<String>,
    }

    impl<'ast> Visit<'ast> for RuntimeHandoffGuard {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(segment) = path.segments.last() {
                match segment.ident.to_string().as_str() {
                    "CaptureCoordinator" => self.coordinator_type_seen = true,
                    "reserve_capture_catalog" => self.coordinator_reservations += 1,
                    "reserve_capture_catalog_until" => {
                        self.coordinator_reservations += 1;
                        self.deadline_aware_coordinator_reservations += 1;
                    }
                    _ => {}
                }
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let method = call.method.to_string();
            if matches!(method.as_str(), "execute" | "execute_preapplied") {
                self.coordinator_execution_calls += 1;
            }
            if method == "execute_preapplied" {
                self.preapplied_execution_calls += 1;
            }
            let receiver =
                receiver_name(&call.receiver).unwrap_or_else(|| "<expression>".to_string());
            if matches!(method.as_str(), "apply" | "complete" | "finalize") {
                self.direct_effect_calls.push(format!(
                    "direct .{method}(…) on {receiver}; use CaptureCoordinator::execute"
                ));
            }
            if method == "write" && (receiver.contains("checkpoint") || receiver.contains("store"))
            {
                self.direct_effect_calls.push(format!(
                    "direct checkpoint/store .write(…) on {receiver}; use CaptureCoordinator::execute"
                ));
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    // ADR-ACF-10 (ACF-19/ACF-20): the validated handoff spans the live
    // pipeline and the checkpoint writers. "execute_preapplied only after
    // reserve_capture_catalog_until" is judged over the five functions
    // together, and the checkpoint stage itself must execute the coordinator.
    let (runtime_path, runtime_file) =
        parse_rust_source("src/internal/ai/capture/live_pipeline.rs");
    let (_, checkpoint_file) = parse_rust_source("src/internal/ai/capture/live_checkpoint.rs");
    let mut runtime_guard = RuntimeHandoffGuard {
        coordinator_type_seen: false,
        coordinator_execution_calls: 0,
        preapplied_execution_calls: 0,
        coordinator_reservations: 0,
        deadline_aware_coordinator_reservations: 0,
        direct_effect_calls: Vec::new(),
    };
    runtime_guard.visit_item_fn(top_level_function(
        &runtime_file,
        "ingest_agent_traces_payload_with_scope",
    ));
    for writer in [
        "write_committed_checkpoint",
        "write_subagent_checkpoint",
        "run_export_stage",
        "run_checkpoint_stage",
    ] {
        runtime_guard.visit_item_fn(top_level_function(&checkpoint_file, writer));
    }
    let mut checkpoint_stage_guard = RuntimeHandoffGuard {
        coordinator_type_seen: false,
        coordinator_execution_calls: 0,
        preapplied_execution_calls: 0,
        coordinator_reservations: 0,
        deadline_aware_coordinator_reservations: 0,
        direct_effect_calls: Vec::new(),
    };
    checkpoint_stage_guard
        .visit_item_fn(top_level_function(&checkpoint_file, "run_checkpoint_stage"));
    assert!(
        checkpoint_stage_guard.preapplied_execution_calls >= 1,
        "run_checkpoint_stage must hand its reservation to CaptureCoordinator::execute_preapplied"
    );
    assert!(
        runtime_guard.coordinator_type_seen,
        "{} capture persistence handoff must construct or receive CaptureCoordinator",
        runtime_path.display()
    );
    assert!(
        runtime_guard.coordinator_execution_calls >= 1,
        "{} capture persistence handoff must delegate ordering through CaptureCoordinator::execute or its pre-reserved continuation",
        runtime_path.display()
    );
    if runtime_guard.preapplied_execution_calls != 0 {
        assert!(
            runtime_guard.coordinator_reservations >= 1,
            "{} may use execute_preapplied only after a coordinator-owned catalog reservation",
            runtime_path.display()
        );
        assert!(
            runtime_guard.deadline_aware_coordinator_reservations >= 1,
            "{} may use execute_preapplied only after reserve_capture_catalog_until has retained the capture deadline",
            runtime_path.display()
        );
    }
    assert!(
        runtime_guard.direct_effect_calls.is_empty(),
        "{} capture persistence handoff must not manually orchestrate stores:\n{}",
        runtime_path.display(),
        runtime_guard.direct_effect_calls.join("\n")
    );

    // --- ADR-ACF-10 (ACF-20) "Runtime inventory": exact non-test item set.
    const RUNTIME: &str = "src/internal/ai/hooks/runtime.rs";
    let runtime_source =
        fs::read_to_string(repo_root().join(RUNTIME)).expect("read hook runtime source");
    let runtime_ast = syn::parse_file(&runtime_source).expect("parse hook runtime source");
    let mut functions = BTreeSet::new();
    let mut types = BTreeSet::new();
    let mut impl_types = BTreeSet::new();
    let mut exported = BTreeSet::new();
    let mut test_exported = BTreeSet::new();
    let mut other_items = Vec::new();
    for item in &runtime_ast.items {
        let test = has_exact_cfg_test(syn_item_attrs(item));
        match item {
            syn::Item::Use(item) if !matches!(item.vis, syn::Visibility::Inherited) => {
                let mut paths = Vec::new();
                flatten_syn_use(&item.tree, "", &mut paths);
                if test {
                    test_exported.extend(paths);
                } else {
                    exported.extend(paths);
                }
            }
            _ if test => {}
            syn::Item::Use(_) => {}
            syn::Item::Fn(item) => {
                functions.insert(item.sig.ident.to_string());
            }
            syn::Item::Struct(item) => {
                types.insert(item.ident.to_string());
            }
            syn::Item::Enum(item) => {
                types.insert(item.ident.to_string());
            }
            syn::Item::Impl(item) => {
                impl_types.insert(match item.self_ty.as_ref() {
                    syn::Type::Path(path) => syn_path_text(&path.path),
                    _ => "<type>".to_string(),
                });
            }
            other => other_items.push(match other {
                syn::Item::Const(item) => format!("const {}", item.ident),
                syn::Item::Static(item) => format!("static {}", item.ident),
                syn::Item::Trait(item) => format!("trait {}", item.ident),
                syn::Item::Macro(item) => format!(
                    "macro {}",
                    item.ident
                        .as_ref()
                        .map_or_else(|| syn_path_text(&item.mac.path), ToString::to_string)
                ),
                syn::Item::Mod(item) => format!("mod {}", item.ident),
                syn::Item::Type(item) => format!("type {}", item.ident),
                _ => "other item".to_string(),
            }),
        }
    }
    let set = |names: &[&str]| -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    };
    assert_eq!(
        functions,
        set(&[
            "advisory_no_evidence_error",
            "await_hook_capture_with_deadline",
            "classify_capture_ingress_for_target",
            "effective_hook_capture_deadline",
            "process_hook_event_from_stdin",
            "process_hook_event_with_target",
            "terminal_persistence_failure_error",
            "validate_capture_ingress_from_stdin",
        ]),
        "{RUNTIME}: non-test function inventory"
    );
    let inventory_types = set(&[
        "HookAdvisoryNoEvidence",
        "HookTarget",
        "HookTerminalPersistenceFailure",
    ]);
    assert_eq!(types, inventory_types, "{RUNTIME}: non-test type inventory");
    assert!(
        impl_types.is_subset(&inventory_types),
        "{RUNTIME}: impl blocks only for the inventory types: {impl_types:?}"
    );
    assert!(
        other_items.is_empty(),
        "{RUNTIME}: no other const/static/trait/macro/mod/type item: {other_items:?}"
    );
    assert_eq!(
        exported,
        set(&[
            "crate::internal::ai::capture::checkpoint::AgentCheckpointRow",
            "crate::internal::ai::capture::checkpoint::SubagentCheckpointRow",
            "crate::internal::ai::capture::checkpoint::insert_agent_checkpoint_row_idempotent",
            "crate::internal::ai::capture::checkpoint::insert_subagent_checkpoint_row_idempotent",
            "crate::internal::ai::capture::ingress::HookEnvelopeInvalid",
            "crate::internal::ai::capture::key::CAPTURE_UNSUPPORTED_PLATFORM_REMEDY",
            "crate::internal::ai::capture::key::CaptureSourceCommitmentDomain",
            "crate::internal::ai::capture::key::derive_capture_source_commitment_in_scope_until",
            "crate::internal::ai::capture::key::derive_snapshot_content_commitment_in_scope_until",
            "crate::internal::ai::capture::live::build_ai_session_id",
            "crate::internal::ai::capture::scope_binding::CAPTURE_SCOPE_BINDING_HELPER_ARG",
            "crate::internal::ai::capture::scope_binding::CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP",
            "crate::internal::ai::capture::scope_binding::CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP",
            "crate::internal::ai::capture::scope_binding::is_capture_unsupported_platform_error",
            "crate::internal::ai::capture::scope_binding::run_capture_scope_binding_helper",
            "crate::internal::ai::capture::scope_binding::run_capture_scope_binding_helper_to_writer",
            "crate::internal::ai::capture::scope_binding::unsupported_platform_scope_binding_failure",
            "super::intent::AI_SESSION_SCHEMA",
            "super::intent::AI_SESSION_TYPE",
        ]),
        "{RUNTIME}: frozen pub/pub(crate) re-export set"
    );
    assert_eq!(
        test_exported,
        set(&["super::intent::append_normalized_event"]),
        "{RUNTIME}: the only test re-export"
    );

    // --- "Line-count method": every non-test top-level item spans
    // `last_line - first_line + 1` lines, where first_line is the smallest of
    // its outer attributes/docs and its first token; the sum is capped.
    fn production_item_lines(file: &syn::File) -> usize {
        use syn::spanned::Spanned;
        file.items
            .iter()
            .filter(|item| !has_exact_cfg_test(syn_item_attrs(item)))
            .map(|item| {
                let span = item.span();
                let first = syn_item_attrs(item)
                    .iter()
                    .map(|attr| attr.span().start().line)
                    .chain(std::iter::once(span.start().line))
                    .min()
                    .unwrap_or(span.start().line);
                span.end().line - first + 1
            })
            .sum()
    }
    let fixture = syn::parse_file(
        "/// counted doc line\nfn counted() {\n}\n#[cfg(test)]\nmod tests {\n    fn skipped() {}\n}\n",
    )
    .expect("parse line-count fixture");
    assert_eq!(
        production_item_lines(&fixture),
        3,
        "line-count self-test: doc + two-line fn, cfg(test) module excluded"
    );
    let runtime_lines = production_item_lines(&runtime_ast);
    println!("{RUNTIME}: {runtime_lines} non-test production lines (cap 480)");
    assert!(
        runtime_lines <= 480,
        "{RUNTIME}: {runtime_lines} non-test production lines exceed the ADR-ACF-10 cap of 480"
    );

    // --- "Runtime dependency ban": exact syn path segments / identifiers in
    // non-test code (attributes and docs are not scanned), plus SQL-shaped
    // literals, macro token trees included.
    #[derive(Default)]
    struct RuntimeDependencyBan {
        hits: Vec<String>,
    }
    impl RuntimeDependencyBan {
        fn banned(identifier: &str) -> bool {
            matches!(
                identifier,
                "sea_orm"
                    | "DatabaseConnection"
                    | "ConnectionTrait"
                    | "Statement"
                    | "history"
                    | "traces"
                    | "coverage_gate"
                    | "export_job"
                    | "subagent_content"
                    | "SessionStore"
                    | "ClientStorage"
                    | "write_git_object"
                    | "TracesCheckpointStore"
                    | "CheckpointWriteRequest"
                    | "CaptureSnapshotService"
                    | "reduce_lifecycle"
                    | "authorized_read"
                    | "AgentKind"
                    | "agent_for"
                    | "live_capture_for"
            ) || identifier.starts_with("CaptureCoordinator")
                || identifier.starts_with("CaptureCatalog")
        }

        fn segments(&mut self, segments: &[String]) {
            for segment in segments {
                if Self::banned(segment) {
                    self.hits.push(segment.clone());
                }
            }
            for pair in segments.windows(2) {
                if matches!(
                    (pair[0].as_str(), pair[1].as_str()),
                    ("internal", "db") | ("tokio", "process") | ("hooks", "providers")
                ) {
                    self.hits.push(format!("{}::{}", pair[0], pair[1]));
                }
            }
        }

        fn literal(&mut self, value: &str) {
            let value = value.trim_start();
            if ["SELECT", "INSERT", "UPDATE", "DELETE", "WITH", "PRAGMA"]
                .iter()
                .any(|keyword| {
                    value
                        .strip_prefix(keyword)
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
                })
            {
                self.hits.push(format!("SQL-shaped literal {value:?}"));
            }
        }

        fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
            let mut path: Vec<String> = Vec::new();
            let mut colons = 0;
            let flush = |this: &mut Self, path: &mut Vec<String>| {
                this.segments(path);
                path.clear();
            };
            for token in tokens {
                match token {
                    proc_macro2::TokenTree::Group(group) => {
                        flush(self, &mut path);
                        self.tokens(group.stream());
                    }
                    proc_macro2::TokenTree::Ident(identifier) => {
                        if colons != 2 {
                            flush(self, &mut path);
                        }
                        path.push(identifier.to_string());
                        colons = 0;
                    }
                    proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ':' => colons += 1,
                    proc_macro2::TokenTree::Punct(_) => {
                        flush(self, &mut path);
                        colons = 0;
                    }
                    proc_macro2::TokenTree::Literal(literal) => {
                        flush(self, &mut path);
                        if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                            self.literal(&value.value());
                        }
                    }
                }
            }
            flush(self, &mut path);
        }
    }
    impl<'ast> Visit<'ast> for RuntimeDependencyBan {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if !has_exact_cfg_test(syn_item_attrs(item)) {
                syn::visit::visit_item(self, item);
            }
        }

        fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
            if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
                syn::visit::visit_stmt(self, stmt);
            }
        }

        fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                let segments: Vec<String> = path.split("::").map(str::to_string).collect();
                self.segments(&segments);
            }
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let segments: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            self.segments(&segments);
            syn::visit::visit_path(self, path);
        }

        fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
            // Bindings, fields and method names are identifiers outside a
            // path; path segments are judged (with their neighbours) above.
            let text = identifier.to_string();
            if Self::banned(&text) {
                self.hits.push(format!("identifier {text}"));
            }
        }

        fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
            self.literal(&literal.value());
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            syn::visit::visit_macro(self, mac);
            self.tokens(mac.tokens.clone());
        }
    }
    let dependency_hits = |source: &str| {
        let mut ban = RuntimeDependencyBan::default();
        ban.visit_file(&syn::parse_file(source).expect("parse dependency-ban source"));
        ban.hits
    };
    let banned_fixture = dependency_hits(
        r#"
        use sea_orm::DatabaseConnection;
        use crate::internal::ai::hooks::providers::claude_provider;
        fn regressed() {
            let _ = crate::internal::db::get_db_conn_instance;
            let _ = "SELECT 1 FROM agent_session";
            tracing::warn!(kind = ?AgentKind::Codex, "skipped");
            let _ = CaptureCoordinatorRequest::new;
        }
        "#,
    );
    for expected in [
        "sea_orm",
        "DatabaseConnection",
        "hooks::providers",
        "internal::db",
        "SQL-shaped literal \"SELECT 1 FROM agent_session\"",
        "AgentKind",
        "CaptureCoordinatorRequest",
    ] {
        assert!(
            banned_fixture.iter().any(|hit| hit == expected),
            "dependency-ban self-test must report {expected}: {banned_fixture:?}"
        );
    }
    let allowed_fixture = dependency_hits(
        r#"
        /// Writes AgentTraces through `traces` and `history` (prose).
        fn neutral(target: HookTarget) -> bool {
            let _ = ingest_agent_traces;
            let _ = "Delete the stale marker";
            target == HookTarget::AgentTraces
        }
        #[cfg(test)]
        mod tests { use crate::internal::ai::hooks::providers::claude_provider; }
        "#,
    );
    assert!(
        allowed_fixture.is_empty(),
        "dependency-ban self-test must not match substrings, prose or tests: {allowed_fixture:?}"
    );
    let runtime_hits = dependency_hits(&runtime_source);
    assert!(
        runtime_hits.is_empty(),
        "{RUNTIME} must only frame IO, resolve the provider once, hand off and map output: {runtime_hits:?}"
    );

    // --- Ordering and the single provider lookup in the hook entry.
    let entry = top_level_function(&runtime_ast, "process_hook_event_with_target");
    let entry_text = {
        use syn::spanned::Spanned;
        span_source_text(&runtime_source, entry.span())
    };
    let (runtime_production, _) = runtime_source
        .split_once("#[cfg(test)]\npub(crate) mod tests")
        .expect("runtime test module delimiter exists");
    assert_eq!(
        runtime_production
            .matches("LiveCaptureBinding::resolve(")
            .count(),
        1,
        "{RUNTIME}: the provider is resolved exactly once"
    );
    let position = |needle: &str| {
        entry_text
            .find(needle)
            .unwrap_or_else(|| panic!("process_hook_event_with_target must call {needle}"))
    };
    let ordered = [
        "new_ingest_span(",
        "LiveCaptureBinding::resolve(provider)",
        "effective_hook_capture_deadline(",
        "validate_capture_ingress_from_stdin(",
        "classify_capture_ingress_for_target(",
        "await_hook_capture_with_deadline(",
    ];
    for pair in ordered.windows(2) {
        assert!(
            position(pair[0]) < position(pair[1]),
            "process_hook_event_with_target must call {} before {}",
            pair[0],
            pair[1]
        );
    }

    // --- No `export_job` path in hooks/** or the live pipeline/checkpoint
    // modules; the only capture-layer owner is ExportJobLeaseStore.
    #[derive(Default)]
    struct ExportJobPaths {
        test_depth: usize,
        impl_owner: Vec<String>,
        hits: Vec<String>,
    }
    impl<'ast> Visit<'ast> for ExportJobPaths {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let test = has_exact_cfg_test(syn_item_attrs(item));
            self.test_depth += usize::from(test);
            syn::visit::visit_item(self, item);
            self.test_depth -= usize::from(test);
        }

        fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
            if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
                syn::visit::visit_stmt(self, stmt);
            }
        }

        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            let owner = match item.self_ty.as_ref() {
                syn::Type::Path(path) => syn_path_text(&path.path),
                _ => "<type>".to_string(),
            };
            self.impl_owner.push(owner);
            syn::visit::visit_item_impl(self, item);
            self.impl_owner.pop();
        }

        fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

        fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
            if self.test_depth == 0 && identifier == "export_job" {
                self.hits.push(
                    self.impl_owner
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "<free item>".to_string()),
                );
            }
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            fn named(tokens: proc_macro2::TokenStream) -> usize {
                tokens
                    .into_iter()
                    .map(|token| match token {
                        proc_macro2::TokenTree::Group(group) => named(group.stream()),
                        proc_macro2::TokenTree::Ident(identifier) => {
                            usize::from(identifier == "export_job")
                        }
                        _ => 0,
                    })
                    .sum()
            }
            syn::visit::visit_macro(self, mac);
            if self.test_depth == 0 {
                for _ in 0..named(mac.tokens.clone()) {
                    self.hits.push("<macro>".to_string());
                }
            }
        }
    }
    let export_job_hits = |relative_path: &str| {
        let (_, file) = parse_rust_source(relative_path);
        let mut scan = ExportJobPaths::default();
        scan.visit_file(&file);
        scan.hits
    };
    let mut export_job_scope = rust_sources_under("src/internal/ai/hooks");
    export_job_scope.extend(
        [
            "src/internal/ai/capture/live_pipeline.rs",
            "src/internal/ai/capture/live_checkpoint.rs",
        ]
        .map(String::from),
    );
    let mut export_job_violations = Vec::new();
    for relative_path in &export_job_scope {
        for owner in export_job_hits(relative_path) {
            export_job_violations.push(format!("{relative_path}: export_job in {owner}"));
        }
    }
    assert!(
        export_job_violations.is_empty(),
        "hooks/** and the live pipeline/checkpoint modules must reach the export job only through LiveExportLeasePort:\n{}",
        export_job_violations.join("\n")
    );
    let live_export_job = export_job_hits("src/internal/ai/capture/live.rs");
    assert!(
        !live_export_job.is_empty()
            && live_export_job
                .iter()
                .all(|owner| owner == "ExportJobLeaseStore"),
        "capture/live.rs names export_job only inside ExportJobLeaseStore (positive anchor): {live_export_job:?}"
    );
}

/// ADR-ACF-10 (ACF-20) "Provider-neutral scan set 與規則": rules (a)–(d)
/// over non-`#[cfg(test)]` code. Attributes (docs included) are never
/// scanned, macro token trees are walked, matching is case-insensitive, and a
/// violation is a source line holding at least one hit.
#[derive(Default)]
struct NeutralRuntimeScan {
    hits: Vec<(usize, String)>,
}

impl NeutralRuntimeScan {
    fn scan(source: &str) -> Self {
        let file = syn::parse_file(source).expect("parse provider-neutral scan source");
        let mut scan = Self::default();
        syn::visit::Visit::visit_file(&mut scan, &file);
        scan
    }

    fn lines(&self) -> BTreeSet<usize> {
        self.hits.iter().map(|(line, _)| *line).collect()
    }

    fn hit(&mut self, span: proc_macro2::Span, what: String) {
        self.hits.push((span.start().line, what));
    }

    /// (a) a string literal holding a provider token.
    fn literal(&mut self, value: &str, span: proc_macro2::Span) {
        if literal_has_provider_token(value) {
            self.hit(span, format!("(a) literal {value:?}"));
        }
    }

    /// (b) an identifier naming a provider; (c) a provider-to-kind dispatch.
    fn identifier(&mut self, identifier: &proc_macro2::Ident) {
        let text = identifier.to_string();
        let text = text.strip_prefix("r#").unwrap_or(&text);
        let lower = text.to_ascii_lowercase();
        if ["claude", "codex", "opencode", "gemini", "copilot"]
            .iter()
            .any(|name| lower.contains(name))
        {
            self.hit(identifier.span(), format!("(b) identifier {text}"));
        }
        if matches!(
            text,
            "AgentKind" | "agent_for" | "live_capture_for" | "from_db_str" | "from_cli_slug"
        ) {
            self.hit(identifier.span(), format!("(c) {text}"));
        }
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Ident(identifier) => self.identifier(&identifier),
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                        self.literal(&value.value(), literal.span());
                    }
                }
                proc_macro2::TokenTree::Punct(_) => {}
            }
        }
    }

    /// (d) a compared or matched string literal: a hit when it holds a
    /// provider token or the other operand / scrutinee names a kind, a
    /// provider or an agent.
    fn compared(&mut self, literal: &syn::LitStr, other: &syn::Expr) {
        if literal_has_provider_token(&literal.value()) || names_kind_provider_or_agent(other) {
            self.hit(
                literal.span(),
                format!("(d) compared literal {:?}", literal.value()),
            );
        }
    }
}

/// The operand itself is a string literal expression (not a call argument).
fn string_literal_expr(expr: &syn::Expr) -> Option<&syn::LitStr> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(literal),
            ..
        }) => Some(literal),
        _ => None,
    }
}

/// String literals that are themselves a match/`matches!` arm pattern.
fn string_literal_patterns(pattern: &syn::Pat, output: &mut Vec<syn::LitStr>) {
    match pattern {
        syn::Pat::Lit(syn::ExprLit {
            lit: syn::Lit::Str(literal),
            ..
        }) => output.push(literal.clone()),
        syn::Pat::Or(or) => {
            for case in &or.cases {
                string_literal_patterns(case, output);
            }
        }
        _ => {}
    }
}

fn names_kind_provider_or_agent(expr: &syn::Expr) -> bool {
    use syn::visit::Visit;

    fn role(identifier: &str) -> bool {
        let lower = identifier.to_ascii_lowercase();
        ["kind", "provider", "agent"]
            .iter()
            .any(|word| lower.contains(word))
    }

    fn tokens(stream: proc_macro2::TokenStream) -> bool {
        stream.into_iter().any(|token| match token {
            proc_macro2::TokenTree::Group(group) => tokens(group.stream()),
            proc_macro2::TokenTree::Ident(identifier) => role(&identifier.to_string()),
            _ => false,
        })
    }

    struct Finder(bool);
    impl<'ast> Visit<'ast> for Finder {
        fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
            self.0 |= role(&identifier.to_string());
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            syn::visit::visit_macro(self, mac);
            self.0 |= tokens(mac.tokens.clone());
        }
    }

    let mut finder = Finder(false);
    finder.visit_expr(expr);
    finder.0
}

/// `matches!(scrutinee, pattern [if guard] [,])`.
fn parse_matches_body(input: syn::parse::ParseStream<'_>) -> syn::Result<(syn::Expr, syn::Pat)> {
    let scrutinee: syn::Expr = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    let pattern = syn::Pat::parse_multi_with_leading_vert(input)?;
    if input.peek(syn::Token![if]) {
        input.parse::<syn::Token![if]>()?;
        input.parse::<syn::Expr>()?;
    }
    if input.peek(syn::Token![,]) {
        input.parse::<syn::Token![,]>()?;
    }
    Ok((scrutinee, pattern))
}

impl<'ast> syn::visit::Visit<'ast> for NeutralRuntimeScan {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !has_exact_cfg_test(syn_item_attrs(item)) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Const(item) => &item.attrs,
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Type(item) => &item.attrs,
            syn::ImplItem::Macro(item) => &item.attrs,
            _ => &[],
        };
        if !has_exact_cfg_test(attrs) {
            syn::visit::visit_impl_item(self, item);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if !has_exact_cfg_test(&arm.attrs) {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        if !has_exact_cfg_test(&variant.attrs) {
            syn::visit::visit_variant(self, variant);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if !has_exact_cfg_test(&field.attrs) {
            syn::visit::visit_field(self, field);
        }
    }

    // Attribute and doc text is prose, never a dispatch decision.
    fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

    fn visit_ident(&mut self, identifier: &'ast proc_macro2::Ident) {
        self.identifier(identifier);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.literal(&literal.value(), literal.span());
    }

    fn visit_expr_binary(&mut self, expr: &'ast syn::ExprBinary) {
        if matches!(expr.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            if let Some(literal) = string_literal_expr(&expr.left) {
                self.compared(literal, &expr.right);
            }
            if let Some(literal) = string_literal_expr(&expr.right) {
                self.compared(literal, &expr.left);
            }
        }
        syn::visit::visit_expr_binary(self, expr);
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        let mut literals = Vec::new();
        for arm in &expr.arms {
            if !has_exact_cfg_test(&arm.attrs) {
                string_literal_patterns(&arm.pat, &mut literals);
            }
        }
        for literal in &literals {
            self.compared(literal, &expr.expr);
        }
        syn::visit::visit_expr_match(self, expr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        syn::visit::visit_macro(self, mac);
        self.tokens(mac.tokens.clone());
        // Rule (d) inside macro arguments: `matches!` is a match; any other
        // comma-separated expression list (`assert!`, `format!`, ...) is
        // visited as ordinary expressions.
        if mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "matches")
        {
            if let Ok((scrutinee, pattern)) = mac.parse_body_with(parse_matches_body) {
                let mut literals = Vec::new();
                string_literal_patterns(&pattern, &mut literals);
                for literal in &literals {
                    self.compared(literal, &scrutinee);
                }
                self.visit_expr(&scrutinee);
            }
        } else if let Ok(arguments) = mac.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            for argument in &arguments {
                self.visit_expr(argument);
            }
        }
    }
}

/// Frozen residual baselines (ADR-ACF-10): non-test `AgentKind::` paths,
/// macro token trees included.
#[derive(Default)]
struct AgentKindPathCount(usize);

impl<'ast> syn::visit::Visit<'ast> for AgentKindPathCount {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !has_exact_cfg_test(syn_item_attrs(item)) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if !has_exact_cfg_test(&arm.attrs) {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let leading = path.segments.len().saturating_sub(1);
        if path
            .segments
            .iter()
            .take(leading)
            .any(|segment| segment.ident == "AgentKind")
        {
            self.0 += 1;
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        fn count(tokens: proc_macro2::TokenStream) -> usize {
            let tokens: Vec<_> = tokens.into_iter().collect();
            let mut total = 0;
            for (index, token) in tokens.iter().enumerate() {
                match token {
                    proc_macro2::TokenTree::Group(group) => total += count(group.stream()),
                    proc_macro2::TokenTree::Ident(identifier) if identifier == "AgentKind" => {
                        if matches!(
                            (tokens.get(index + 1), tokens.get(index + 2)),
                            (
                                Some(proc_macro2::TokenTree::Punct(first)),
                                Some(proc_macro2::TokenTree::Punct(second))
                            ) if first.as_char() == ':' && second.as_char() == ':'
                        ) {
                            total += 1;
                        }
                    }
                    _ => {}
                }
            }
            total
        }
        syn::visit::visit_macro(self, mac);
        self.0 += count(mac.tokens.clone());
    }
}

fn agent_kind_path_count(source: &str) -> usize {
    let file = syn::parse_file(source).expect("parse AgentKind ratchet source");
    let mut count = AgentKindPathCount::default();
    syn::visit::Visit::visit_file(&mut count, &file);
    count.0
}

/// ADR-ACF-10 (ACF-20): the shared live runtime — `hooks/{runtime,intent}.rs`
/// and `capture/{coordinator,live,scope_binding,live_pipeline,live_checkpoint}.rs`
/// — holds no provider token, name or kind dispatch in non-test code, and
/// takes transcript export only through the `LiveTranscriptExporter`
/// capability. The scanner is self-tested on the ADR fixtures (positive
/// exactly 4 lines, negative exactly 0), and the frozen residual
/// `AgentKind::` baselines of `capture/{snapshot,extraction}.rs` may only
/// fall.
#[test]
fn live_capture_shared_runtime_is_provider_neutral() {
    let positive = NeutralRuntimeScan::scan(
        r#"
        fn regressed(agent_kind: &str) {
            let _ = matches!(agent_kind, "claude_code" | "codex");
            tracing::warn!("opencode export …");
            if agent_kind == "opencode" {}
            let _ = AgentKind::OpenCode;
        }
        "#,
    );
    assert_eq!(
        positive.lines().len(),
        4,
        "provider-neutral self-test (positive): {:?}",
        positive.hits
    );
    let negative = NeutralRuntimeScan::scan(
        r#"
        fn neutral(phase: &str, message: Message, value: Value, kind: &str, event: Event) {
            match phase { "active" => () }
            let _ = message.role == "user";
            let _ = value.get("kind").and_then(Value::as_str) == Some(kind);
            let _ = match event.kind { SubagentStart => "start", _ => "end" };
        }
        "#,
    );
    assert!(
        negative.lines().is_empty(),
        "provider-neutral self-test (negative): {:?}",
        negative.hits
    );
    // Prose and test-only code are outside the scan, while a macro argument
    // and a role-named scrutinee are inside it.
    let scoped = NeutralRuntimeScan::scan(
        r#"
        /// The Claude transcript and the OpenCode exporter (prose).
        fn documented() {}
        #[cfg(test)]
        mod tests { fn fixture() { let _ = "claude_code"; } }
        fn regressed(provider_kind: &str, verb: &str) {
            assert!(provider_kind != "unknown");
            match verb { "stop" => (), _ => () }
        }
        "#,
    );
    assert_eq!(
        scoped.lines().len(),
        1,
        "provider-neutral self-test (scope): {:?}",
        scoped.hits
    );

    let scan_set = [
        "src/internal/ai/hooks/runtime.rs",
        "src/internal/ai/hooks/intent.rs",
        "src/internal/ai/capture/coordinator.rs",
        "src/internal/ai/capture/live.rs",
        "src/internal/ai/capture/scope_binding.rs",
        "src/internal/ai/capture/live_pipeline.rs",
        "src/internal/ai/capture/live_checkpoint.rs",
    ];
    let mut violations = Vec::new();
    for relative_path in scan_set {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        let scan = NeutralRuntimeScan::scan(&source);
        let lines = scan.lines();
        violations.extend(
            scan.hits
                .iter()
                .filter(|(line, _)| lines.contains(line))
                .map(|(line, hit)| format!("{relative_path}:{line}: {hit}")),
        );
    }
    assert!(
        violations.is_empty(),
        "the shared live runtime must take provider decisions from LiveCaptureProvider, never provider tokens ({} hits):\n{}",
        violations.len(),
        violations.join("\n")
    );

    // AC3: transcript export reaches the shared runtime only through the
    // exporter capability.
    let live_checkpoint =
        fs::read_to_string(repo_root().join("src/internal/ai/capture/live_checkpoint.rs"))
            .expect("read live checkpoint source");
    let live_checkpoint_file =
        syn::parse_file(&live_checkpoint).expect("parse live checkpoint source");
    {
        use syn::spanned::Spanned;
        let writer = span_source_text(
            &live_checkpoint,
            top_level_function(&live_checkpoint_file, "write_committed_checkpoint").span(),
        );
        let export_stage = span_source_text(
            &live_checkpoint,
            top_level_function(&live_checkpoint_file, "run_export_stage").span(),
        );
        assert!(
            writer.contains("binding.transcript_exporter()")
                && export_stage.contains("exporter.export(context, export_deadline.monotonic())")
                && export_stage.contains("exporter.export_coverage_normalizer()"),
            "transcript export must come from the binding's LiveTranscriptExporter capability"
        );
    }

    // Frozen residual baselines (DEFER-ACF-06): only allowed to fall.
    let ratchet_fixture = agent_kind_path_count(
        r#"
        fn dispatch(kind: AgentKind) -> u8 {
            let _ = matches!(kind, AgentKind::Codex);
            match kind { AgentKind::ClaudeCode => 1, AgentKind::Codex | AgentKind::OpenCode => 2, _ => 0 }
        }
        #[cfg(test)]
        mod tests { fn fixture() { let _ = AgentKind::Codex; } }
        "#,
    );
    assert_eq!(ratchet_fixture, 4, "AgentKind ratchet self-test");
    for (relative_path, baseline) in [
        ("src/internal/ai/capture/snapshot.rs", 1),
        ("src/internal/ai/capture/extraction.rs", 19),
    ] {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        let count = agent_kind_path_count(&source);
        println!("{relative_path}: {count} non-test AgentKind:: paths (baseline {baseline})");
        assert!(
            count <= baseline,
            "{relative_path}: non-test AgentKind:: paths rose to {count} (frozen baseline {baseline}; DEFER-ACF-06 may only lower it)"
        );
    }
}

/// Name of the top-level or impl function enclosing each visited node.
#[derive(Default)]
struct EnclosingFunctions(Vec<String>);

impl EnclosingFunctions {
    fn current(&self) -> String {
        self.0
            .last()
            .cloned()
            .unwrap_or_else(|| "<module>".to_string())
    }
}

/// Positions of the early exits (`return`, `?`, `bail!`) a function body can
/// take outside any `async` block or closure (an inner future or a deferred
/// result constructor).
#[derive(Default)]
struct EarlyExits {
    nested: usize,
    exits: Vec<(proc_macro2::LineColumn, &'static str)>,
}

impl<'ast> syn::visit::Visit<'ast> for EarlyExits {
    fn visit_expr_async(&mut self, expr: &'ast syn::ExprAsync) {
        self.nested += 1;
        syn::visit::visit_expr_async(self, expr);
        self.nested -= 1;
    }

    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        self.nested += 1;
        syn::visit::visit_expr_closure(self, expr);
        self.nested -= 1;
    }

    fn visit_expr_return(&mut self, expr: &'ast syn::ExprReturn) {
        use syn::spanned::Spanned;
        if self.nested == 0 {
            self.exits.push((expr.span().start(), "return"));
        }
        syn::visit::visit_expr_return(self, expr);
    }

    fn visit_expr_try(&mut self, expr: &'ast syn::ExprTry) {
        if self.nested == 0 {
            self.exits.push((expr.question_token.spans[0].start(), "?"));
        }
        syn::visit::visit_expr_try(self, expr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        use syn::spanned::Spanned;
        if self.nested == 0
            && mac
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "bail")
        {
            self.exits.push((mac.span().start(), "bail!"));
        }
        syn::visit::visit_macro(self, mac);
    }
}

fn line_column_key(at: proc_macro2::LineColumn) -> (usize, usize) {
    (at.line, at.column)
}

/// How a stage function uses its export runner token (`runner`).
#[derive(Default)]
struct RunnerTokenUses {
    nested: usize,
    settle: Vec<proc_macro2::LineColumn>,
    borrows: usize,
    handoffs: usize,
    scrutinees: usize,
    violations: Vec<String>,
}

fn is_runner_token(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Path(path) if path.path.is_ident("runner"))
}

impl<'ast> syn::visit::Visit<'ast> for RunnerTokenUses {
    fn visit_expr_async(&mut self, expr: &'ast syn::ExprAsync) {
        self.nested += 1;
        syn::visit::visit_expr_async(self, expr);
        self.nested -= 1;
    }

    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        self.nested += 1;
        syn::visit::visit_expr_closure(self, expr);
        self.nested -= 1;
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if is_runner_token(&call.receiver) {
            let method = call.method.to_string();
            if self.nested > 0 {
                self.violations.push(format!(
                    "runner.{method}(..) inside an inner future/closure"
                ));
            }
            match method.as_str() {
                "settle" => self.settle.push(call.method.span().start()),
                "owner" | "now_ms" | "is_some" => self.borrows += 1,
                other => self
                    .violations
                    .push(format!("runner.{other}(..) is not a borrow or settle")),
            }
            for argument in &call.args {
                self.visit_expr(argument);
            }
            return;
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        for field in &expr.fields {
            let named_runner =
                matches!(&field.member, syn::Member::Named(name) if name == "runner");
            if named_runner && is_runner_token(&field.expr) {
                let proceed = expr
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Proceed");
                if proceed && self.nested == 0 {
                    self.handoffs += 1;
                } else {
                    self.violations.push(
                        "runner moved into a struct other than ExportStage::Proceed".to_string(),
                    );
                }
            } else {
                self.visit_expr(&field.expr);
            }
        }
        if let Some(rest) = &expr.rest {
            self.visit_expr(rest);
        }
    }

    fn visit_expr_let(&mut self, expr: &'ast syn::ExprLet) {
        if is_runner_token(&expr.expr) {
            if self.nested > 0 {
                self.violations
                    .push("runner matched inside an inner future/closure".to_string());
            }
            self.scrutinees += 1;
            return;
        }
        syn::visit::visit_expr_let(self, expr);
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        if is_runner_token(&expr.expr) {
            self.violations
                .push("runner matched by `match`; use one `if let` settlement".to_string());
        }
        syn::visit::visit_expr_match(self, expr);
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        if expr.path.is_ident("runner") {
            self.violations
                .push("runner moved outside settle / ExportStage::Proceed".to_string());
        }
    }

    fn visit_pat(&mut self, _pattern: &'ast syn::Pat) {}
}

/// `#[name(..)]` list contents of an attribute.
fn attribute_list(attr: &syn::Attribute, name: &str) -> Option<String> {
    match &attr.meta {
        syn::Meta::List(list) if list.path.is_ident(name) => Some(list.tokens.to_string()),
        _ => None,
    }
}

/// Every call of `callee` inside `function`: (direct `.await` operands,
/// syn-visible calls, `callee(` occurrences in the token stream — macro
/// arguments included).
fn awaited_call_counts(
    source: &str,
    function: &syn::ItemFn,
    callee: &str,
) -> (usize, usize, usize) {
    use syn::{spanned::Spanned, visit::Visit};

    fn named(expr: &syn::Expr, callee: &str) -> bool {
        matches!(expr, syn::Expr::Call(call) if matches!(
            call.func.as_ref(),
            syn::Expr::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == callee)
        ))
    }

    struct Counter<'c> {
        callee: &'c str,
        direct: usize,
        calls: usize,
    }

    impl<'ast> Visit<'ast> for Counter<'_> {
        fn visit_expr_await(&mut self, expr: &'ast syn::ExprAwait) {
            if named(&expr.base, self.callee) {
                self.direct += 1;
            }
            syn::visit::visit_expr_await(self, expr);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if matches!(
                call.func.as_ref(),
                syn::Expr::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == self.callee)
            ) {
                self.calls += 1;
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    fn token_calls(tokens: proc_macro2::TokenStream, callee: &str) -> usize {
        let tokens: Vec<_> = tokens.into_iter().collect();
        let mut total = 0;
        for (index, token) in tokens.iter().enumerate() {
            match token {
                proc_macro2::TokenTree::Group(group) => {
                    total += token_calls(group.stream(), callee)
                }
                proc_macro2::TokenTree::Ident(identifier) if identifier == callee => {
                    if matches!(
                        tokens.get(index + 1),
                        Some(proc_macro2::TokenTree::Group(group))
                            if group.delimiter() == proc_macro2::Delimiter::Parenthesis
                    ) {
                        total += 1;
                    }
                }
                _ => {}
            }
        }
        total
    }

    let mut counter = Counter {
        callee,
        direct: 0,
        calls: 0,
    };
    counter.visit_block(&function.block);
    let block_tokens = span_source_text(source, function.block.span())
        .parse::<proc_macro2::TokenStream>()
        .expect("re-tokenize a parsed function body");
    let tokens = token_calls(block_tokens, callee);
    (counter.direct, counter.calls, tokens)
}

/// ADR-ACF-10 (ACF-20) "機器偵測": the export runner token is settled at
/// exactly two sites, never escapes its inner future, and its holder can
/// never be cancelled.
///
/// - `LiveExportRunner::settle` (defined once, consuming `self`) is called by
///   non-test code exactly once in `run_export_stage` and once in
///   `run_checkpoint_stage`;
/// - between the token's binding and `settle`, neither stage exits early
///   (`return` / `?` / `bail!`) outside its inner future, and
///   `write_committed_checkpoint` exits nowhere between the
///   `ExportStage::Proceed` handoff and `run_checkpoint_stage`;
/// - the token is only borrowed (`owner` / `now_ms` / `is_some`), settled,
///   or handed off through `ExportStage::Proceed` / the writer's one
///   `Some(runner)` into `run_checkpoint_stage`, and never named inside an
///   inner future or closure;
/// - the lease port's release/advance methods are called only inside
///   `settle`; `settle`, the port and both stages call no
///   `abandon_reserved_turn_claims*` directly; the port has no claim method;
/// - every `CheckpointStageExit` variant is built only by its top-level class
///   helper;
/// - the token is neither `Clone` nor `Copy` and panics on an unsettled drop
///   in debug builds only;
/// - the holder chain from the hook commands to both stages is a chain of
///   direct `.await` operands, with the AiIntent-only timeout in
///   `await_hook_capture_with_deadline` as the single wrapped use.
#[test]
fn capture_export_runner_token_settles_at_two_sites() {
    use syn::{spanned::Spanned, visit::Visit};

    // --- Settlement and lease-port call sites across non-test src.
    #[derive(Default)]
    struct CallSites {
        test_depth: usize,
        functions: EnclosingFunctions,
        calls: Vec<(String, String)>,
    }

    impl CallSites {
        fn function<F: FnOnce(&mut Self)>(
            &mut self,
            name: String,
            attrs: &[syn::Attribute],
            visit: F,
        ) {
            let test = has_exact_cfg_test(attrs);
            self.test_depth += usize::from(test);
            self.functions.0.push(name);
            visit(self);
            self.functions.0.pop();
            self.test_depth -= usize::from(test);
        }
    }

    impl<'ast> Visit<'ast> for CallSites {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let test = has_exact_cfg_test(syn_item_attrs(item));
            self.test_depth += usize::from(test);
            syn::visit::visit_item(self, item);
            self.test_depth -= usize::from(test);
        }

        fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
            if !has_exact_cfg_test(syn_stmt_attrs(stmt)) {
                syn::visit::visit_stmt(self, stmt);
            }
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            self.function(item.sig.ident.to_string(), &item.attrs, |this| {
                syn::visit::visit_item_fn(this, item);
            });
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            self.function(item.sig.ident.to_string(), &item.attrs, |this| {
                syn::visit::visit_impl_item_fn(this, item);
            });
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let method = call.method.to_string();
            if self.test_depth == 0
                && matches!(
                    method.as_str(),
                    "settle" | "release_failed" | "release_dirty" | "advance_and_release"
                )
            {
                self.calls.push((method, self.functions.current()));
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    let mut settle_sites = Vec::new();
    let mut port_sites = Vec::new();
    for relative_path in rust_sources_under("src") {
        let stem = relative_path.trim_end_matches(".rs");
        if stem.ends_with("tests") || stem.ends_with("_test") {
            continue;
        }
        let (_, file) = parse_rust_source(&relative_path);
        let mut sites = CallSites::default();
        sites.visit_file(&file);
        for (method, function) in sites.calls {
            let site = format!("{relative_path}::{function}");
            if method == "settle" {
                settle_sites.push(site);
            } else {
                port_sites.push((method, site));
            }
        }
    }
    settle_sites.sort();
    assert_eq!(
        settle_sites,
        [
            "src/internal/ai/capture/live_checkpoint.rs::run_checkpoint_stage",
            "src/internal/ai/capture/live_checkpoint.rs::run_export_stage",
        ],
        "LiveExportRunner::settle has exactly two non-test call sites"
    );
    let port_methods: BTreeSet<&str> = port_sites
        .iter()
        .map(|(method, _)| method.as_str())
        .collect();
    assert_eq!(
        port_methods,
        BTreeSet::from(["advance_and_release", "release_dirty", "release_failed"]),
        "every lease release/advance method has a settlement call site"
    );
    for (method, site) in &port_sites {
        assert_eq!(
            site, "src/internal/ai/capture/live.rs::settle",
            "the lease port's {method} may be called only by LiveExportRunner::settle"
        );
    }

    // `settle` is the token's single terminal method and consumes it.
    const LIVE: &str = "src/internal/ai/capture/live.rs";
    const LIVE_CHECKPOINT: &str = "src/internal/ai/capture/live_checkpoint.rs";
    let live_source = fs::read_to_string(repo_root().join(LIVE)).expect("read capture live source");
    let live_file = syn::parse_file(&live_source).expect("parse capture live source");
    let impl_self = |item: &syn::ItemImpl| match item.self_ty.as_ref() {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    let impl_trait = |item: &syn::ItemImpl| {
        item.trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .map(|segment| segment.ident.to_string())
    };
    let runner_impls: Vec<&syn::ItemImpl> = live_file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(item) if impl_self(item) == "LiveExportRunner" => Some(item),
            _ => None,
        })
        .collect();
    let inherent_methods: Vec<&syn::ImplItemFn> = runner_impls
        .iter()
        .filter(|item| item.trait_.is_none())
        .flat_map(|item| item.items.iter())
        .filter_map(|item| match item {
            syn::ImplItem::Fn(method) => Some(method),
            _ => None,
        })
        .collect();
    let consuming: Vec<String> = inherent_methods
        .iter()
        .filter(|method| {
            matches!(
                method.sig.receiver(),
                Some(receiver) if receiver.reference.is_none()
            )
        })
        .map(|method| method.sig.ident.to_string())
        .collect();
    assert_eq!(
        consuming,
        ["settle"],
        "settle is the runner token's only consuming method"
    );
    let settle = inherent_methods
        .iter()
        .find(|method| method.sig.ident == "settle")
        .expect("LiveExportRunner::settle exists");
    let settle_text = span_source_text(&live_source, settle.span());
    assert!(
        settle_text.contains("self.mark_settled();")
            && settle_text.find("self.mark_settled();") < settle_text.find(".await"),
        "settle marks the token consumed before any await"
    );

    // Not Clone/Copy; a debug-only drop check that never fires mid-unwind.
    let runner_struct = top_level_struct(&live_file, "LiveExportRunner");
    for derived in runner_struct
        .attrs
        .iter()
        .filter_map(|attr| attribute_list(attr, "derive"))
    {
        assert!(
            !derived.contains("Clone") && !derived.contains("Copy"),
            "LiveExportRunner must be neither Clone nor Copy: {derived}"
        );
    }
    let settled_field = runner_struct
        .fields
        .iter()
        .find(|field| field.ident.as_ref().is_some_and(|ident| ident == "settled"))
        .expect("LiveExportRunner carries its consumed flag");
    assert!(
        settled_field
            .attrs
            .iter()
            .any(|attr| attribute_list(attr, "cfg").as_deref() == Some("debug_assertions")),
        "the consumed flag exists only in debug builds"
    );
    let trait_impls: Vec<(String, &syn::ItemImpl)> = runner_impls
        .iter()
        .filter_map(|item| impl_trait(item).map(|name| (name, *item)))
        .collect();
    assert!(
        trait_impls
            .iter()
            .all(|(name, _)| !matches!(name.as_str(), "Clone" | "Copy")),
        "LiveExportRunner must be neither Clone nor Copy"
    );
    let drops: Vec<&syn::ItemImpl> = trait_impls
        .iter()
        .filter(|(name, _)| name == "Drop")
        .map(|(_, item)| *item)
        .collect();
    let [drop_impl] = drops.as_slice() else {
        panic!(
            "LiveExportRunner has exactly one Drop impl, found {}",
            drops.len()
        );
    };
    assert!(
        drop_impl
            .attrs
            .iter()
            .any(|attr| attribute_list(attr, "cfg").as_deref() == Some("debug_assertions")),
        "the unsettled-token drop check exists only in debug builds"
    );
    let drop_text = span_source_text(&live_source, drop_impl.span());
    assert!(
        drop_text.contains("!self.settled")
            && drop_text.contains("!std::thread::panicking()")
            && drop_text.contains("panic!("),
        "an unsettled token panics on drop unless the thread is already unwinding"
    );

    // The lease port has no claim method.
    let port = live_file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Trait(item) if item.ident == "LiveExportLeasePort" => Some(item),
            _ => None,
        })
        .expect("capture/live.rs defines LiveExportLeasePort");
    let port_method_names: BTreeSet<String> = port
        .items
        .iter()
        .filter_map(|item| match item {
            syn::TraitItem::Fn(method) => Some(method.sig.ident.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        port_method_names,
        BTreeSet::from(
            [
                "advance_and_release",
                "observe_idle",
                "release_dirty",
                "release_failed"
            ]
            .map(String::from)
        ),
        "LiveExportLeasePort carries only the export-job lease methods"
    );
    for item in &port.items {
        if let syn::TraitItem::Fn(method) = item {
            let signature = span_source_text(&live_source, method.sig.span()).to_ascii_lowercase();
            assert!(
                !signature.contains("claim"),
                "LiveExportLeasePort::{} must not take or name coverage claims",
                method.sig.ident
            );
        }
    }

    // No direct claim abandonment in settle, the port or either stage.
    #[derive(Default)]
    struct Abandons(Vec<String>);
    impl<'ast> Visit<'ast> for Abandons {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(segment) = path.segments.last()
                && segment
                    .ident
                    .to_string()
                    .starts_with("abandon_reserved_turn_claims")
            {
                self.0.push(segment.ident.to_string());
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call
                .method
                .to_string()
                .starts_with("abandon_reserved_turn_claims")
            {
                self.0.push(call.method.to_string());
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }
    let live_checkpoint =
        fs::read_to_string(repo_root().join(LIVE_CHECKPOINT)).expect("read live checkpoint source");
    let live_checkpoint_file =
        syn::parse_file(&live_checkpoint).expect("parse live checkpoint source");
    let export_stage = top_level_function(&live_checkpoint_file, "run_export_stage");
    let checkpoint_stage = top_level_function(&live_checkpoint_file, "run_checkpoint_stage");
    let mut abandons = Abandons::default();
    abandons.visit_impl_item_fn(settle);
    abandons.visit_item_trait(port);
    for item in &live_file.items {
        if let syn::Item::Impl(item) = item
            && impl_trait(item).as_deref() == Some("LiveExportLeasePort")
        {
            abandons.visit_item_impl(item);
        }
    }
    abandons.visit_item_fn(export_stage);
    abandons.visit_item_fn(checkpoint_stage);
    assert!(
        abandons.0.is_empty(),
        "settle, the lease port and the stages must not abandon claims directly: {:?}",
        abandons.0
    );
    let mut indirect = Abandons::default();
    indirect.visit_item_fn(top_level_function(
        &live_checkpoint_file,
        "release_reserved_checkpoint_side_effects",
    ));
    assert!(
        !indirect.0.is_empty(),
        "claim cleanup stays in release_reserved_checkpoint_side_effects (scanner positive control)"
    );

    // No early exit between the token binding and settle.
    fn exits_between(
        function: &syn::ItemFn,
        after: proc_macro2::LineColumn,
        before: proc_macro2::LineColumn,
    ) -> Vec<String> {
        let mut exits = EarlyExits::default();
        syn::visit::Visit::visit_block(&mut exits, &function.block);
        exits
            .exits
            .iter()
            .filter(|(at, _)| {
                line_column_key(after) < line_column_key(*at)
                    && line_column_key(*at) < line_column_key(before)
            })
            .map(|(at, kind)| format!("{kind} at {}:{}", at.line, at.column))
            .collect()
    }
    let export_binding = export_stage
        .block
        .stmts
        .iter()
        .find_map(|stmt| match stmt {
            syn::Stmt::Local(local)
                if matches!(&local.pat, syn::Pat::Ident(binding) if binding.ident == "runner") =>
            {
                Some(local.span().end())
            }
            _ => None,
        })
        .expect("run_export_stage binds its admitted runner");
    for (function, binding) in [
        (export_stage, export_binding),
        (
            checkpoint_stage,
            checkpoint_stage.block.brace_token.span.open().start(),
        ),
    ] {
        let mut uses = RunnerTokenUses::default();
        uses.visit_block(&function.block);
        let [settle_at] = uses.settle.as_slice() else {
            panic!(
                "{} settles its runner exactly once, found {}",
                function.sig.ident,
                uses.settle.len()
            );
        };
        let exits = exits_between(function, binding, *settle_at);
        assert!(
            exits.is_empty(),
            "{} exits early between its runner binding and settle outside the inner future: {exits:?}",
            function.sig.ident
        );
    }

    // The token is only borrowed, settled or handed off.
    let mut export_uses = RunnerTokenUses::default();
    for stmt in &export_stage.block.stmts {
        let binds_runner = matches!(
            stmt,
            syn::Stmt::Local(local)
                if matches!(&local.pat, syn::Pat::Ident(binding) if binding.ident == "runner")
        );
        if !binds_runner {
            export_uses.visit_stmt(stmt);
        }
    }
    assert!(
        export_uses.violations.is_empty()
            && export_uses.settle.len() == 1
            && export_uses.handoffs == 1
            && export_uses.scrutinees == 0
            && export_uses.borrows >= 1,
        "run_export_stage must only borrow, settle or hand off its runner: {:?} (settle {}, handoffs {}, scrutinees {})",
        export_uses.violations,
        export_uses.settle.len(),
        export_uses.handoffs,
        export_uses.scrutinees
    );
    let mut checkpoint_uses = RunnerTokenUses::default();
    checkpoint_uses.visit_block(&checkpoint_stage.block);
    assert!(
        checkpoint_uses.violations.is_empty()
            && checkpoint_uses.settle.len() == 1
            && checkpoint_uses.handoffs == 0
            && checkpoint_uses.scrutinees == 1,
        "run_checkpoint_stage must only borrow its runner and settle it once: {:?} (settle {}, handoffs {}, scrutinees {})",
        checkpoint_uses.violations,
        checkpoint_uses.settle.len(),
        checkpoint_uses.handoffs,
        checkpoint_uses.scrutinees
    );

    // The writer's single handoff: ExportStage::Proceed → Some(runner) →
    // run_checkpoint_stage, with no early exit in between.
    let writer = top_level_function(&live_checkpoint_file, "write_committed_checkpoint");
    #[derive(Default)]
    struct Handoff {
        proceed_binding: Vec<proc_macro2::LineColumn>,
        runner_uses: Vec<String>,
        export_runner_uses: Vec<String>,
        stage_call: Vec<proc_macro2::LineColumn>,
        context: Vec<&'static str>,
    }
    impl<'ast> Visit<'ast> for Handoff {
        fn visit_pat_struct(&mut self, pattern: &'ast syn::PatStruct) {
            if pattern
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Proceed")
                && pattern.fields.iter().any(
                    |field| matches!(&field.member, syn::Member::Named(name) if name == "runner"),
                )
            {
                self.proceed_binding.push(pattern.span().end());
            }
            syn::visit::visit_pat_struct(self, pattern);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            let callee = match call.func.as_ref() {
                syn::Expr::Path(path) => path
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            if callee == "run_checkpoint_stage" {
                self.stage_call.push(call.span().start());
            }
            let context = match callee.as_str() {
                "Some" => "Some(..)",
                "run_checkpoint_stage" => "run_checkpoint_stage(..)",
                _ => "call",
            };
            self.context.push(context);
            syn::visit::visit_expr_call(self, call);
            self.context.pop();
        }

        fn visit_expr_assign(&mut self, expr: &'ast syn::ExprAssign) {
            if matches!(expr.left.as_ref(), syn::Expr::Path(path) if path.path.is_ident("export_runner"))
            {
                self.export_runner_uses.push("assigned".to_string());
                self.visit_expr(&expr.right);
                return;
            }
            syn::visit::visit_expr_assign(self, expr);
        }

        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            let context = self.context.last().copied().unwrap_or("bare").to_string();
            if expr.path.is_ident("runner") {
                self.runner_uses.push(context);
            } else if expr.path.is_ident("export_runner") {
                self.export_runner_uses.push(context);
            }
        }
    }
    let mut handoff = Handoff::default();
    handoff.visit_item_fn(writer);
    let ([proceed], [stage_call]) = (
        handoff.proceed_binding.as_slice(),
        handoff.stage_call.as_slice(),
    ) else {
        panic!(
            "write_committed_checkpoint binds one ExportStage::Proceed runner and calls run_checkpoint_stage once"
        );
    };
    assert_eq!(
        handoff.runner_uses,
        ["Some(..)"],
        "the Proceed runner moves only into Some(runner)"
    );
    assert_eq!(
        handoff.export_runner_uses,
        ["assigned", "run_checkpoint_stage(..)"],
        "the handed-off runner moves only into run_checkpoint_stage"
    );
    let exits = exits_between(writer, *proceed, *stage_call);
    assert!(
        exits.is_empty(),
        "write_committed_checkpoint may not exit between the Proceed handoff and run_checkpoint_stage: {exits:?}"
    );

    // Each CheckpointStageExit variant is built only by its class helper.
    #[derive(Default)]
    struct VariantBuilds {
        test_depth: usize,
        nested: usize,
        functions: EnclosingFunctions,
        builds: Vec<(String, String, bool)>,
    }
    impl VariantBuilds {
        fn record(&mut self, path: &syn::Path) {
            let segments: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if self.test_depth == 0
                && let [enum_name, variant] = segments.as_slice()
                && enum_name == "CheckpointStageExit"
            {
                self.builds
                    .push((variant.clone(), self.functions.current(), self.nested > 0));
            }
        }
    }
    impl<'ast> Visit<'ast> for VariantBuilds {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let test = has_exact_cfg_test(syn_item_attrs(item));
            self.test_depth += usize::from(test);
            syn::visit::visit_item(self, item);
            self.test_depth -= usize::from(test);
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            let nested = !self.functions.0.is_empty();
            self.nested += usize::from(nested);
            self.functions.0.push(item.sig.ident.to_string());
            syn::visit::visit_item_fn(self, item);
            self.functions.0.pop();
            self.nested -= usize::from(nested);
        }

        fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
            self.nested += 1;
            syn::visit::visit_expr_closure(self, expr);
            self.nested -= 1;
        }

        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            self.record(&expr.path);
        }

        fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
            self.record(&expr.path);
            syn::visit::visit_expr_struct(self, expr);
        }

        // Patterns match an exit; they never build one.
        fn visit_pat(&mut self, _pattern: &'ast syn::Pat) {}
    }
    let checkpoint_exit = live_checkpoint_file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "CheckpointStageExit" => Some(item),
            _ => None,
        })
        .expect("capture/live_checkpoint.rs defines CheckpointStageExit");
    let helpers = [
        ("Authorized", "authorized"),
        ("Expire", "expire"),
        ("PostCheckpoint", "post_checkpoint"),
        ("Uncommitted", "uncommitted"),
        ("UncommittedSettle", "uncommitted_settle"),
    ];
    assert_eq!(
        checkpoint_exit
            .variants
            .iter()
            .map(|variant| variant.ident.to_string())
            .collect::<BTreeSet<_>>(),
        helpers
            .iter()
            .map(|(variant, _)| variant.to_string())
            .collect::<BTreeSet<_>>(),
        "every CheckpointStageExit variant has a class helper"
    );
    let mut builds = VariantBuilds::default();
    builds.visit_file(&live_checkpoint_file);
    let mut expected: Vec<(String, String, bool)> = helpers
        .iter()
        .map(|(variant, helper)| (variant.to_string(), helper.to_string(), false))
        .collect();
    expected.sort();
    builds.builds.sort();
    assert_eq!(
        builds.builds, expected,
        "each CheckpointStageExit variant is built exactly once, by its top-level class helper"
    );
    for (_, helper) in helpers {
        top_level_function(&live_checkpoint_file, helper);
    }

    // The holder chain is a chain of direct `.await` operands.
    const RUNTIME: &str = "src/internal/ai/hooks/runtime.rs";
    const LIVE_PIPELINE: &str = "src/internal/ai/capture/live_pipeline.rs";
    let runtime_source = fs::read_to_string(repo_root().join(RUNTIME)).expect("read hook runtime");
    let runtime_file = syn::parse_file(&runtime_source).expect("parse hook runtime");
    let live_pipeline_source =
        fs::read_to_string(repo_root().join(LIVE_PIPELINE)).expect("read live pipeline");
    let live_pipeline_file = syn::parse_file(&live_pipeline_source).expect("parse live pipeline");
    for (source, file, relative_path, function, callee) in [
        (
            &runtime_source,
            &runtime_file,
            RUNTIME,
            "process_hook_event_with_target",
            "ingest_agent_traces",
        ),
        (
            &runtime_source,
            &runtime_file,
            RUNTIME,
            "process_hook_event_with_target",
            "await_hook_capture_with_deadline",
        ),
        (
            &live_pipeline_source,
            &live_pipeline_file,
            LIVE_PIPELINE,
            "ingest_agent_traces",
            "ingest_agent_traces_payload_with_scope",
        ),
        (
            &live_pipeline_source,
            &live_pipeline_file,
            LIVE_PIPELINE,
            "ingest_agent_traces_payload_with_scope",
            "write_committed_checkpoint",
        ),
        (
            &live_checkpoint,
            &live_checkpoint_file,
            LIVE_CHECKPOINT,
            "write_committed_checkpoint",
            "run_export_stage",
        ),
        (
            &live_checkpoint,
            &live_checkpoint_file,
            LIVE_CHECKPOINT,
            "write_committed_checkpoint",
            "run_checkpoint_stage",
        ),
    ] {
        let (direct, calls, tokens) =
            awaited_call_counts(source, top_level_function(file, function), callee);
        assert!(
            direct >= 1 && direct == calls && calls == tokens,
            "{relative_path}::{function} must call {callee} only as a direct `.await` operand (direct {direct}, calls {calls}, token calls {tokens})"
        );
    }
    for relative_path in ["src/command/hooks.rs", "src/command/agent/hooks.rs"] {
        let source = fs::read_to_string(repo_root().join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {relative_path}: {error}"));
        let mut direct_total = 0;
        for item in &file.items {
            if let syn::Item::Fn(function) = item
                && !has_exact_cfg_test(&function.attrs)
            {
                let (direct, calls, tokens) =
                    awaited_call_counts(&source, function, "process_hook_event_with_target");
                assert!(
                    direct == calls && calls == tokens,
                    "{relative_path}::{} must await process_hook_event_with_target directly (direct {direct}, calls {calls}, token calls {tokens})",
                    function.sig.ident
                );
                direct_total += direct;
            }
        }
        assert!(
            direct_total >= 1,
            "{relative_path} must enter the hook runtime"
        );
    }

    // `capture` (the AgentTraces holder) is only ever awaited bare; the one
    // timeout-wrapped use is confined to the AiIntent branch.
    #[derive(Default)]
    struct CaptureUses {
        intent_branch: usize,
        bare_awaits: usize,
        intent_timeouts: usize,
        violations: Vec<String>,
    }
    fn is_capture(expr: &syn::Expr) -> bool {
        matches!(expr, syn::Expr::Path(path) if path.path.is_ident("capture"))
    }
    impl<'ast> Visit<'ast> for CaptureUses {
        fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
            let intent = matches!(
                expr.cond.as_ref(),
                syn::Expr::Binary(binary)
                    if matches!(binary.op, syn::BinOp::Eq(_))
                        && matches!(binary.left.as_ref(), syn::Expr::Path(path) if path.path.is_ident("target"))
                        && matches!(binary.right.as_ref(), syn::Expr::Path(path) if syn_path_text(&path.path) == "HookTarget::AiIntent")
            );
            self.visit_expr(&expr.cond);
            self.intent_branch += usize::from(intent);
            self.visit_block(&expr.then_branch);
            self.intent_branch -= usize::from(intent);
            if let Some((_, otherwise)) = &expr.else_branch {
                self.visit_expr(otherwise);
            }
        }

        fn visit_expr_await(&mut self, expr: &'ast syn::ExprAwait) {
            if is_capture(&expr.base) {
                self.bare_awaits += 1;
                return;
            }
            syn::visit::visit_expr_await(self, expr);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            let timeout = matches!(
                call.func.as_ref(),
                syn::Expr::Path(path) if path.path.segments.last().is_some_and(|segment| {
                    segment.ident == "timeout" || segment.ident == "timeout_at"
                })
            );
            for argument in &call.args {
                if is_capture(argument) {
                    if timeout && self.intent_branch > 0 {
                        self.intent_timeouts += 1;
                    } else {
                        self.violations.push(
                            "capture passed to a call outside the AiIntent timeout".to_string(),
                        );
                    }
                } else {
                    self.visit_expr(argument);
                }
            }
            self.visit_expr(&call.func);
        }

        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            if expr.path.is_ident("capture") {
                self.violations
                    .push("capture used other than as a bare `.await`".to_string());
            }
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if mac
                .tokens
                .clone()
                .into_iter()
                .any(|token| matches!(token, proc_macro2::TokenTree::Ident(identifier) if identifier == "capture"))
            {
                self.violations
                    .push("capture named inside a macro (select!/spawn)".to_string());
            }
            syn::visit::visit_macro(self, mac);
        }
    }
    let mut capture_uses = CaptureUses::default();
    capture_uses.visit_item_fn(top_level_function(
        &runtime_file,
        "await_hook_capture_with_deadline",
    ));
    assert!(
        capture_uses.violations.is_empty()
            && capture_uses.bare_awaits >= 2
            && capture_uses.intent_timeouts == 1,
        "await_hook_capture_with_deadline may wrap `capture` only in the AiIntent timeout: {:?} (bare awaits {}, intent timeouts {})",
        capture_uses.violations,
        capture_uses.bare_awaits,
        capture_uses.intent_timeouts
    );
    // The entry hands its `capture` future only to that wrapper.
    #[derive(Default)]
    struct CaptureHandoff {
        handoffs: usize,
        others: usize,
    }
    impl<'ast> Visit<'ast> for CaptureHandoff {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            let wrapper = matches!(
                call.func.as_ref(),
                syn::Expr::Path(path) if path.path.is_ident("await_hook_capture_with_deadline")
            );
            for argument in &call.args {
                if is_capture(argument) && wrapper {
                    self.handoffs += 1;
                } else {
                    self.visit_expr(argument);
                }
            }
        }

        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            if expr.path.is_ident("capture") {
                self.others += 1;
            }
        }

        fn visit_pat(&mut self, _pattern: &'ast syn::Pat) {}
    }
    let mut capture_handoff = CaptureHandoff::default();
    capture_handoff.visit_item_fn(top_level_function(
        &runtime_file,
        "process_hook_event_with_target",
    ));
    assert_eq!(
        (capture_handoff.handoffs, capture_handoff.others),
        (1, 0),
        "process_hook_event_with_target hands `capture` only to await_hook_capture_with_deadline"
    );
}

/// ACF-07 has one terminal-finalization policy.  Provider hooks may configure
/// their own transport timeout, but they must never own a capture receipt or
/// terminal decision state.  They may inject the common policy as an opaque
/// value. The runtime injects that policy into the coordinator, and every
/// downstream provider plan records the same rule before its production cards
/// are unblocked.
#[test]
fn provider_plans_consume_common_finalizer_policy() {
    use syn::visit::Visit;

    struct FinalizerPolicyGuard {
        violations: Vec<String>,
        policy_fields: Option<BTreeSet<String>>,
    }

    impl<'ast> Visit<'ast> for FinalizerPolicyGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            if item.ident == "CaptureFinalizePolicy" {
                let syn::Fields::Named(fields) = &item.fields else {
                    self.violations.push(
                        "CaptureFinalizePolicy must retain named, auditable fields".to_string(),
                    );
                    return;
                };
                self.policy_fields = Some(
                    fields
                        .named
                        .iter()
                        .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
                        .collect(),
                );
            }
            syn::visit::visit_item_struct(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let rendered = syn_path_text(path);
            let normalized = rendered.strip_prefix("crate::").unwrap_or(&rendered);
            if normalized.starts_with("internal::ai::observed_agents")
                || normalized.starts_with("ai::observed_agents")
                || normalized.starts_with("internal::ai::hooks::providers")
                || normalized.starts_with("ai::hooks::providers")
                || matches!(normalized, "AgentKind" | "ObservedAgent" | "HookProvider")
            {
                self.violations.push(format!(
                    "finalizer policy acquired provider identity/dependency {rendered}"
                ));
            }
            syn::visit::visit_path(self, path);
        }
    }

    let (finalizer_path, finalizer_file) =
        parse_rust_source("src/internal/ai/capture/finalizer.rs");
    let mut finalizer_guard = FinalizerPolicyGuard {
        violations: Vec::new(),
        policy_fields: None,
    };
    finalizer_guard.visit_file(&finalizer_file);
    assert_eq!(
        finalizer_guard.policy_fields,
        Some(BTreeSet::from([
            "deadline_millis".to_string(),
            "mode".to_string(),
            "replay_key".to_string(),
        ])),
        "{} CaptureFinalizePolicy may contain only deadline, mode, and replay key",
        finalizer_path.display()
    );
    assert!(
        finalizer_guard.violations.is_empty(),
        "{} finalizer policy must remain provider-neutral:\n{}",
        finalizer_path.display(),
        finalizer_guard.violations.join("\n")
    );

    struct PolicyUseGuard {
        policy_uses: usize,
    }

    impl<'ast> Visit<'ast> for PolicyUseGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        // A type position (`CaptureFinalizePolicy`) or an associated call
        // (`CaptureFinalizePolicy::new(..)`) both inject the common policy.
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path
                .segments
                .iter()
                .any(|segment| segment.ident == "CaptureFinalizePolicy")
            {
                self.policy_uses += 1;
            }
            syn::visit::visit_path(self, path);
        }
    }

    // ADR-ACF-10 (ACF-19): the live pipeline and checkpoint writers inject
    // the common policy that the hook runtime used to own.
    for relative_path in [
        "src/internal/ai/capture/coordinator.rs",
        "src/internal/ai/capture/live_pipeline.rs",
        "src/internal/ai/capture/live_checkpoint.rs",
    ] {
        let (path, file) = parse_rust_source(relative_path);
        let mut guard = PolicyUseGuard { policy_uses: 0 };
        guard.visit_file(&file);
        assert!(
            guard.policy_uses > 0,
            "{} must inject/use CaptureFinalizePolicy instead of a local terminal timeout/ledger policy",
            path.display()
        );
    }

    struct ProviderFinalizerGuard {
        violations: Vec<String>,
    }

    impl<'ast> Visit<'ast> for ProviderFinalizerGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_syn_use(&item.tree, "", &mut paths);
            for path in paths {
                if matches!(
                    path.rsplit("::").next(),
                    Some(
                        "PendingFinalizeReceipt"
                            | "FinalizeDecision"
                            | "FinalizeDecisionInput"
                            | "FinalizeCheckpointProgress"
                            | "FinalizePendingStage"
                            | "FinalizeQuarantineReason"
                    )
                ) {
                    self.violations.push(format!(
                        "provider adapter imports terminal receipt/decision implementation {path}"
                    ));
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            let name = item.ident.to_string();
            if name.contains("Finalizer") || name.contains("FinalizeLedger") {
                self.violations.push(format!(
                    "provider adapter defines private terminal state type {name}"
                ));
            }
            syn::visit::visit_item_struct(self, item);
        }

        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            let name = item.ident.to_string();
            if name.contains("Finalizer") || name.contains("FinalizeLedger") {
                self.violations.push(format!(
                    "provider adapter defines private terminal state type {name}"
                ));
            }
            syn::visit::visit_item_enum(self, item);
        }
    }

    let provider_root = repo_root().join("src/internal/ai/hooks/providers");
    let mut stack = vec![provider_root];
    let mut checked = 0usize;
    let mut provider_violations = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).expect("read provider adapter directory") {
            let path = entry.expect("read provider adapter entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read provider adapter source");
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let mut guard = ProviderFinalizerGuard {
                violations: Vec::new(),
            };
            guard.visit_file(&file);
            checked += 1;
            for violation in guard.violations {
                provider_violations.push(format!("{}: {violation}", path.display()));
            }
        }
    }
    assert!(
        provider_violations.is_empty(),
        "provider adapters must not create/import their own capture finalizer:\n{}",
        provider_violations.join("\n")
    );
    assert!(
        checked >= 10,
        "expected to scan provider adapters, scanned only {checked} sources"
    );

    for (plan, gate) in [
        ("plan-20260902.md", "DEP-ACF-MIRROR"),
        ("plan-20260904.md", "DEP-ACF-MIRROR"),
        ("plan-20260905.md", "DEP-ACF-MIRROR"),
        ("plan-20260911.md", "DEP-ACF-MIRROR"),
    ] {
        let path = repo_root().join("docs/development/plan").join(plan);
        let document = fs::read_to_string(&path).expect("read downstream provider plan");
        assert!(
            document.contains(gate),
            "{} must retain the {gate} foundation gate",
            path.display()
        );
        assert!(
            document.contains("CaptureFinalizePolicy"),
            "{} must state that terminal work injects the common CaptureFinalizePolicy",
            path.display()
        );
        assert!(
            document.contains("不得私建 ledger/finalizer"),
            "{} must forbid provider-owned terminal ledger/finalizer state",
            path.display()
        );
    }
}

/// ACF-08/09 close the foundation only when the source layering and every
/// downstream gate agree.  This deliberately combines the two halves: a plan
/// may not advertise an unblocked provider while import still carries a
/// second catalog writer, and a refactor may not hide an unreviewed plan gate
/// behind otherwise-clean Rust code.
#[test]
fn capture_foundation_downstream_plan_and_layering_contract() {
    use syn::visit::Visit;

    struct CaptureModuleGuard {
        modules: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for CaptureModuleGuard {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            self.modules.insert(item.ident.to_string());
            syn::visit::visit_item_mod(self, item);
        }
    }

    let (capture_mod_path, capture_mod) = parse_rust_source("src/internal/ai/capture/mod.rs");
    let mut module_guard = CaptureModuleGuard {
        modules: BTreeSet::new(),
    };
    module_guard.visit_file(&capture_mod);
    let required_modules = BTreeSet::from([
        "catalog".to_string(),
        "checkpoint".to_string(),
        "coordinator".to_string(),
        "finalizer".to_string(),
        "ingress".to_string(),
        "key".to_string(),
        "snapshot".to_string(),
        "state".to_string(),
    ]);
    assert!(
        required_modules.is_subset(&module_guard.modules),
        "{} must continue to expose the complete provider-neutral capture foundation; missing: {:?}",
        capture_mod_path.display(),
        required_modules
            .difference(&module_guard.modules)
            .collect::<Vec<_>>()
    );

    struct ImportLayerGuard {
        catalog_mutations: Vec<String>,
        coverage_claim_mutations: Vec<String>,
        direct_history_append: Vec<String>,
        direct_checkpoint_backend: Vec<String>,
        capture_services: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for ImportLayerGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
            let value = literal.value();
            if let Some(table) = sql_capture_catalog_mutation(&value) {
                self.catalog_mutations.push(format!(
                    "{table} mutation in SQL literal {:?}",
                    value.split_whitespace().collect::<Vec<_>>().join(" ")
                ));
            }
            if let Some(table) = sql_coverage_claim_mutation(&value) {
                self.coverage_claim_mutations.push(format!(
                    "{table} mutation in SQL literal {:?}",
                    value.split_whitespace().collect::<Vec<_>>().join(" ")
                ));
            }
            syn::visit::visit_lit_str(self, literal);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let rendered = syn_path_text(path);
            if rendered.ends_with("::HistoryManager") || rendered == "HistoryManager" {
                self.direct_history_append
                    .push(format!("HistoryManager dependency {rendered}"));
            }
            if rendered.ends_with("::append_checkpoint_commit")
                || rendered == "append_checkpoint_commit"
            {
                self.direct_history_append
                    .push(format!("direct checkpoint append {rendered}"));
            }
            if let Some(last) = path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                && matches!(
                    last.as_str(),
                    "CheckpointCommitParams"
                        | "CheckpointScope"
                        | "ClientStorage"
                        | "HistoryManager"
                        | "TRACES_BRANCH"
                )
            {
                self.direct_checkpoint_backend.push(format!(
                    "direct checkpoint/history backend dependency {rendered}"
                ));
            }
            for segment in &path.segments {
                let name = segment.ident.to_string();
                if matches!(
                    name.as_str(),
                    "CaptureSnapshotService"
                        | "CaptureCatalogStore"
                        | "CaptureCatalogPort"
                        | "CheckpointStore"
                        | "TracesCheckpointStore"
                        | "ImportedCheckpointWriter"
                        | "CaptureCoordinator"
                ) {
                    self.capture_services.insert(name);
                }
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "append_checkpoint_commit" {
                self.direct_history_append
                    .push("direct HistoryManager::append_checkpoint_commit call".to_string());
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    /// ACF-08 deliberately retains the pre-existing, descriptor-pinned import
    /// acquisition seam.  It is not a second persistence path: the helper
    /// consumes that held descriptor once, then hands trusted in-memory bytes
    /// to the shared snapshot service before normalization.  Keep the marker
    /// checks AST-based so aliases, formatting, or a new helper cannot hide a
    /// path reopen or an additional full transcript read.
    #[derive(Default)]
    struct ImportFlowGuard {
        call_paths: Vec<String>,
        method_calls: Vec<String>,
        events: Vec<String>,
        paths: BTreeSet<String>,
        field_accesses: BTreeSet<String>,
        current_dir_calls: usize,
        all_current_dirs_neutral: bool,
    }

    impl ImportFlowGuard {
        fn call_count(&self, name: &str) -> usize {
            self.call_paths
                .iter()
                .filter(|path| path.rsplit("::").next() == Some(name))
                .count()
        }

        fn has_method(&self, name: &str) -> bool {
            self.method_calls.iter().any(|method| method == name)
        }

        fn has_neutral_cwd(&self) -> bool {
            self.current_dir_calls == 1 && self.all_current_dirs_neutral
        }
    }

    impl<'ast> Visit<'ast> for ImportFlowGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_exact_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref() {
                let rendered = syn_path_text(&path.path);
                self.events.push(format!("call:{rendered}"));
                self.call_paths.push(rendered);
            }
            syn::visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let method = call.method.to_string();
            if method == "current_dir" {
                let is_neutral = call.args.len() == 1
                    && matches!(
                        call.args.first(),
                        Some(syn::Expr::Call(path_call))
                            if matches!(path_call.func.as_ref(), syn::Expr::Path(path)
                                if syn_path_text(&path.path) == "std::path::Path::new")
                                && matches!(path_call.args.first(),
                                    Some(syn::Expr::Lit(literal))
                                        if matches!(&literal.lit, syn::Lit::Str(value) if value.value() == "/"))
                    );
                if self.current_dir_calls == 0 {
                    self.all_current_dirs_neutral = true;
                }
                self.current_dir_calls += 1;
                self.all_current_dirs_neutral &= is_neutral;
            }
            self.events.push(format!("method:{method}"));
            self.method_calls.push(method);
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
            if let syn::Member::Named(member) = &field.member {
                self.field_accesses.insert(member.to_string());
            }
            syn::visit::visit_expr_field(self, field);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.paths.insert(syn_path_text(path));
            syn::visit::visit_path(self, path);
        }
    }

    // The matcher must see the claim DML shapes the importer used to spell,
    // while reads used for cursor/plan decisions stay allowed.
    for (sql, expected) in [
        (
            "UPDATE agent_coverage_claim SET state = 'abandoned', owner = NULL WHERE owner = ?",
            Some("agent_coverage_claim"),
        ),
        (
            "INSERT OR IGNORE INTO agent_coverage_claim (session_id) VALUES (?)",
            Some("agent_coverage_claim"),
        ),
        (
            "DELETE FROM agent_coverage_claim WHERE session_id = ?",
            Some("agent_coverage_claim"),
        ),
        (
            "SELECT logical_turn_key FROM agent_coverage_claim WHERE session_id = ?",
            None,
        ),
    ] {
        assert_eq!(
            sql_coverage_claim_mutation(sql).as_deref(),
            expected,
            "{sql}"
        );
    }

    let (import_path, import_file) = parse_rust_source("src/internal/ai/agent_import.rs");
    let mut import_guard = ImportLayerGuard {
        catalog_mutations: Vec::new(),
        coverage_claim_mutations: Vec::new(),
        direct_history_append: Vec::new(),
        direct_checkpoint_backend: Vec::new(),
        capture_services: BTreeSet::new(),
    };
    import_guard.visit_file(&import_file);
    assert!(
        import_guard.catalog_mutations.is_empty(),
        "{} must not retain direct agent_session/agent_checkpoint SQL; pass a typed import request to the capture foundation:\n{}",
        import_path.display(),
        import_guard.catalog_mutations.join("\n")
    );
    assert!(
        import_guard.coverage_claim_mutations.is_empty(),
        "{} must not retain direct agent_coverage_claim SQL; reserve, renew, bind and abandon import claims through the coverage_gate `_with_conn` helpers in the import transaction:\n{}",
        import_path.display(),
        import_guard.coverage_claim_mutations.join("\n")
    );
    assert!(
        import_guard.direct_history_append.is_empty(),
        "{} must route traces writes through CheckpointStore, never HistoryManager directly:\n{}",
        import_path.display(),
        import_guard.direct_history_append.join("\n")
    );
    assert!(
        import_guard.direct_checkpoint_backend.is_empty(),
        "{} must not construct a direct checkpoint/history backend; use ImportedCheckpointWriter through CheckpointStore instead:\n{}",
        import_path.display(),
        import_guard.direct_checkpoint_backend.join("\n")
    );
    for service in [
        "CaptureSnapshotService",
        "CaptureCatalogStore",
        "CheckpointStore",
        "TracesCheckpointStore",
        "ImportedCheckpointWriter",
    ] {
        assert!(
            import_guard.capture_services.contains(service),
            "{} must consume the common {service} seam",
            import_path.display()
        );
    }

    let mut import_flow = ImportFlowGuard::default();
    import_flow.visit_file(&import_file);
    for forbidden_call in [
        "resolve_import_transcript_source_until",
        "resolve_live_transcript_source_until",
        "read_transcript",
        "File::open",
        "OpenOptions::open",
        "std::fs::read",
        "fs::read",
    ] {
        assert!(
            !import_flow.call_paths.iter().any(
                |path| path == forbidden_call || path.ends_with(&format!("::{forbidden_call}"))
            ),
            "{} must not reopen a transcript path through {forbidden_call}; source acquisition belongs to the command's descriptor-pinning stage before the held-FD raw-read helper handoff",
            import_path.display()
        );
    }
    let forbidden_method = "read_bounded";
    assert!(
        !import_flow.has_method(forbidden_method),
        "{} must not add an in-process raw transcript reader through .{forbidden_method}(); use the held-FD helper handoff",
        import_path.display()
    );
    assert!(
        import_flow
            .method_calls
            .iter()
            .filter(|method| method.as_str() == "preview_bounded")
            .count()
            <= 1,
        "{} must not add another legacy descriptor preview reader after its caller has acquired the source",
        import_path.display()
    );
    assert!(
        import_flow.has_method("write_turn"),
        "{} must persist imported checkpoints through ImportedCheckpointWriter::write_turn",
        import_path.display()
    );
    for claim_helper in [
        "reserve_import_turn_claims_until",
        "renew_import_turn_claim_leases_with_conn",
        "bind_import_turn_claim_attempt_with_conn",
        "abandon_reserved_turn_claims_with_conn",
    ] {
        assert!(
            import_flow.call_count(claim_helper) >= 1,
            "{} must delegate its coverage-claim transition to coverage_gate::{claim_helper}",
            import_path.display()
        );
    }

    let source_reader = top_level_function(&import_file, "read_import_source");
    let mut source_reader_guard = ImportFlowGuard::default();
    source_reader_guard.visit_item_fn(source_reader);
    let required_method = "into_rewound_inner";
    assert!(
        source_reader_guard.has_method(required_method),
        "{}::read_import_source must hand its held descriptor to the helper boundary ({required_method})",
        import_path.display()
    );
    assert_eq!(
        source_reader_guard.call_count("read_authorized_descriptor_until"),
        1,
        "{}::read_import_source must hand each held descriptor to exactly one bounded helper",
        import_path.display()
    );
    for forbidden_method in ["read_bounded", "preview_bounded", "read_to_end"] {
        assert!(
            !source_reader_guard.has_method(forbidden_method),
            "{}::read_import_source must not consume the held descriptor through .{forbidden_method}()",
            import_path.display()
        );
    }

    let descriptor_reader = top_level_function(&import_file, "read_authorized_descriptor_until");
    let mut descriptor_reader_guard = ImportFlowGuard::default();
    descriptor_reader_guard.visit_item_fn(descriptor_reader);
    for required_method in ["kill_on_drop", "take", "wait"] {
        assert!(
            descriptor_reader_guard.has_method(required_method),
            "{}::read_authorized_descriptor_until must retain the bounded helper boundary ({required_method})",
            import_path.display()
        );
    }
    assert_eq!(
        descriptor_reader_guard.call_count("read_async_strictly_bounded"),
        1,
        "{}::read_authorized_descriptor_until must drain the helper response through the strict-cap reader",
        import_path.display()
    );
    assert!(
        descriptor_reader_guard
            .paths
            .contains("CancellationSafeChild::new_process_group")
            && descriptor_reader_guard.call_count("configure_private_helper_process_group") == 1
            && descriptor_reader_guard.has_method("terminate_and_reap_checked")
            && descriptor_reader_guard.has_method("env_clear")
            && descriptor_reader_guard.has_method("current_dir")
            && descriptor_reader_guard.has_neutral_cwd(),
        "{}::read_authorized_descriptor_until must use private process containment and a sanitized environment/cwd",
        import_path.display()
    );
    assert!(
        descriptor_reader_guard.call_count("timeout_at") >= 2,
        "{}::read_authorized_descriptor_until must bound both stdout drain and leader wait by the shared deadline",
        import_path.display()
    );

    let preparation = top_level_function(&import_file, "prepare_import_request");
    let mut preparation_guard = ImportFlowGuard::default();
    preparation_guard.visit_item_fn(preparation);
    let snapshot_handoff = preparation_guard
        .events
        .iter()
        .position(|event| event == "method:capture_authorized")
        .expect("historical import preparation must call the shared snapshot service");
    assert!(
        preparation_guard.has_method("into_redacted_transcript"),
        "{}::prepare_import_request must use the redacted snapshot payload rather than raw provider bytes",
        import_path.display()
    );
    for normalizer in [
        "normalize_claude_transcript_until",
        "normalize_codex_rollout_until",
        "normalize_opencode_export_until",
    ] {
        let normalization = preparation_guard
            .events
            .iter()
            .position(|event| event == &format!("call:{normalizer}"))
            .expect("historical import preparation must retain every provider normalizer");
        assert!(
            snapshot_handoff < normalization,
            "{}::prepare_import_request must hand the authorized source to CaptureSnapshotService before {normalizer}",
            import_path.display()
        );
    }
    assert_eq!(
        preparation_guard.call_count("read_import_source"),
        0,
        "{}::prepare_import_request must not trigger a second transcript read after snapshot handoff",
        import_path.display()
    );
    for forbidden_method in ["read_bounded", "preview_bounded", "read_to_end"] {
        assert!(
            !preparation_guard.has_method(forbidden_method),
            "{}::prepare_import_request must not reread the source through .{forbidden_method}() after snapshot handoff",
            import_path.display()
        );
    }

    let (import_command_path, import_command) = parse_rust_source("src/command/agent/import.rs");
    let mut command_flow = ImportFlowGuard::default();
    command_flow.visit_file(&import_command);
    assert_eq!(
        command_flow.call_count("read_import_source"),
        0,
        "{} must not retain the retired raw-byte preparation path; descriptor preparation owns the only transcript read",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("provider_session_id_from_source"),
        0,
        "{} must not use the legacy preview helper in addition to the held-FD import read",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("resolve_import_transcript_source_until"),
        1,
        "{} must retain one descriptor-pinning acquisition path for file-backed imports",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("authorized_trusted_sandboxed_export_until"),
        1,
        "{} must retain the deadline-aware authorized export source boundary for OpenCode imports",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("prepare_candidate_bounded"),
        1,
        "{} must retain one descriptor-pinning preparation path per import candidate",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("run_import_preparation_descriptor_helper_bounded"),
        1,
        "{} must route preparation through exactly one registered descriptor helper boundary",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("derive_capture_source_commitment_in_scope_until"),
        1,
        "{} must derive durable import ownership through the scoped repository-keyed capability",
        import_command_path.display()
    );
    assert_eq!(
        command_flow.call_count("derive_snapshot_content_commitment_in_scope_until"),
        1,
        "{} must derive shared snapshot-content provenance through the typed scoped capability",
        import_command_path.display()
    );

    let candidate_source = top_level_function(&import_command, "resolve_candidate_source");
    let mut candidate_source_guard = ImportFlowGuard::default();
    candidate_source_guard.visit_item_fn(candidate_source);
    for forbidden_method in ["read_bounded", "preview_bounded", "read_to_end"] {
        assert!(
            !candidate_source_guard.has_method(forbidden_method),
            "{}::resolve_candidate_source must pin/export a source without consuming raw transcript bytes through .{forbidden_method}()",
            import_command_path.display()
        );
    }

    let candidate_preparation = top_level_function(&import_command, "prepare_candidate_bounded");
    let mut candidate_preparation_guard = ImportFlowGuard::default();
    candidate_preparation_guard.visit_item_fn(candidate_preparation);
    assert_eq!(
        candidate_preparation_guard.call_count("resolve_candidate_source"),
        1,
        "{}::prepare_candidate_bounded must resolve and pin the candidate source exactly once",
        import_command_path.display()
    );
    assert_eq!(
        candidate_preparation_guard.call_count("run_import_preparation_descriptor_helper_bounded"),
        1,
        "{}::prepare_candidate_bounded must hand the one pinned descriptor to the private helper exactly once",
        import_command_path.display()
    );
    assert_eq!(
        candidate_preparation_guard.call_count("import_source_preimage"),
        1,
        "{}::prepare_candidate_bounded must derive one transient source preimage before durable provenance is minted",
        import_command_path.display()
    );
    assert_eq!(
        candidate_preparation_guard.call_count("derive_capture_source_commitment_in_scope_until"),
        1,
        "{}::prepare_candidate_bounded must mint durable import ownership through the scoped HMAC capability",
        import_command_path.display()
    );
    assert_eq!(
        candidate_preparation_guard.call_count("derive_snapshot_content_commitment_in_scope_until"),
        1,
        "{}::prepare_candidate_bounded must mint snapshot-content provenance through the typed scoped HMAC capability",
        import_command_path.display()
    );
    assert!(
        candidate_preparation_guard
            .paths
            .contains("CaptureSourceCommitmentDomain::ImportSourceV2")
            && candidate_preparation_guard.has_method("assert_workspace_fence_live"),
        "{}::prepare_candidate_bounded must bind import ownership and shared snapshot content commitments to a live capture scope",
        import_command_path.display()
    );
    assert!(
        candidate_preparation_guard.has_method("into_rewound_inner"),
        "{}::prepare_candidate_bounded must pass the pinned file descriptor, rather than reopen a locator, to descriptor preparation",
        import_command_path.display()
    );
    for forbidden_method in ["read_bounded", "preview_bounded", "read_to_end"] {
        assert!(
            !candidate_preparation_guard.has_method(forbidden_method),
            "{}::prepare_candidate_bounded must not consume raw transcript bytes through .{forbidden_method}(); only the descriptor helper may read them",
            import_command_path.display()
        );
    }
    for forbidden_call in [
        "File::open",
        "OpenOptions::open",
        "std::fs::read",
        "fs::read",
    ] {
        assert!(
            !candidate_preparation_guard.call_paths.iter().any(
                |path| path == forbidden_call || path.ends_with(&format!("::{forbidden_call}"))
            ),
            "{}::prepare_candidate_bounded must not reopen a transcript locator through {forbidden_call}",
            import_command_path.display()
        );
    }

    let descriptor_helper = top_level_function(
        &import_command,
        "run_import_preparation_descriptor_helper_from_stdin",
    );
    let mut descriptor_helper_guard = ImportFlowGuard::default();
    descriptor_helper_guard.visit_item_fn(descriptor_helper);
    assert!(
        descriptor_helper_guard.call_count("read_strictly_bounded") == 1
            && descriptor_helper_guard.paths.contains("std::io::stdin"),
        "{} descriptor helper must be the sole strict-cap raw-body reader, consuming its held stdin capability",
        import_command_path.display()
    );
    let preparation_runner = top_level_function(
        &import_command,
        "run_import_preparation_descriptor_helper_bounded",
    );
    let mut preparation_runner_guard = ImportFlowGuard::default();
    preparation_runner_guard.visit_item_fn(preparation_runner);
    assert!(
        preparation_runner_guard.has_method("env_clear")
            && preparation_runner_guard.has_method("current_dir")
            && preparation_runner_guard.has_neutral_cwd(),
        "{} descriptor preparation runner must not inherit caller environment or working directory",
        import_command_path.display()
    );
    assert!(
        !descriptor_helper_guard
            .call_paths
            .iter()
            .any(|path| path.rsplit("::").next() == Some("resolve_candidate_source")),
        "{} descriptor helper must not resolve a new source after the parent pins the descriptor",
        import_command_path.display()
    );

    let public_failure = top_level_function(&import_command, "safe_failure");
    let mut public_failure_guard = ImportFlowGuard::default();
    public_failure_guard.visit_item_fn(public_failure);
    // The documented per-item id is a short unkeyed hash of the provider
    // session id alone (the same report carries full ids for completed
    // items); it must never be derived from a source locator or commitment.
    assert!(
        public_failure_guard
            .field_accesses
            .contains("provider_session_id")
            && public_failure_guard.call_count("digest") == 1
            && public_failure_guard
                .call_paths
                .iter()
                .any(|path| path == "sha2::Sha256::digest"),
        "{}::safe_failure must report the documented short hashed provider session id",
        import_command_path.display()
    );
    for forbidden_field in [
        "path",
        "source_id",
        "source_fingerprint",
        "existing_session_fingerprint",
    ] {
        assert!(
            !public_failure_guard
                .field_accesses
                .contains(forbidden_field),
            "{}::safe_failure must not turn {forbidden_field} into a public correlator",
            import_command_path.display()
        );
    }
    for forbidden_call in [
        "import_provider_commitment",
        "import_source_preimage",
        "derive_capture_source_commitment_in_scope_until",
        "derive_snapshot_content_commitment_in_scope_until",
    ] {
        assert_eq!(
            public_failure_guard.call_count(forbidden_call),
            0,
            "{}::safe_failure must not publish an unkeyed or scoped source hash through {forbidden_call}",
            import_command_path.display()
        );
    }

    let (coverage_path, coverage_file) = parse_rust_source("src/internal/ai/coverage_gate.rs");
    let mut coverage_guard = ImportLayerGuard {
        catalog_mutations: Vec::new(),
        coverage_claim_mutations: Vec::new(),
        direct_history_append: Vec::new(),
        direct_checkpoint_backend: Vec::new(),
        capture_services: BTreeSet::new(),
    };
    coverage_guard.visit_file(&coverage_file);
    assert!(
        coverage_guard.catalog_mutations.is_empty(),
        "{} may own coverage claims but must not mutate agent_session/agent_checkpoint directly:\n{}",
        coverage_path.display(),
        coverage_guard.catalog_mutations.join("\n")
    );
    assert!(
        !coverage_guard.coverage_claim_mutations.is_empty(),
        "{} must remain the owner of the coverage-claim DML the import entrypoint delegates to",
        coverage_path.display()
    );
    assert!(
        coverage_guard.direct_history_append.is_empty(),
        "{} must not bypass CheckpointStore with HistoryManager:\n{}",
        coverage_path.display(),
        coverage_guard.direct_history_append.join("\n")
    );

    let plan_dir = repo_root().join("docs/development/plan");
    let foundation_path = plan_dir.join("plan-20260924.md");
    let foundation =
        fs::read_to_string(&foundation_path).expect("read Session Capture foundation plan");
    // Every foundation card, including the cards split later (G-09) and the
    // baseline test-lock fix, must close before the foundation reads as closed.
    let foundation_tasks = [
        "ACF-01",
        "ACF-02",
        "ACF-03",
        "ACF-04",
        "ACF-05",
        "ACF-06",
        "ACF-07",
        "ACF-08",
        "ACF-09",
        "ACF-10",
        "ACF-11",
        "ACF-12",
        "ACF-13",
        "ACF-14",
        "ACF-15",
        "ACF-16",
        "ACF-17",
        "ACF-18",
        "ACF-19",
        "ACF-20",
        "FIX-ACF-01",
    ];
    let all_task_statuses_done = foundation_tasks.iter().all(|task| {
        task_section(&foundation, task).contains("**Lifecycle / Acceptance:** `done` / `complete`")
    });
    let all_task_checklists_checked = foundation_tasks.iter().all(|task| {
        !task_section(&foundation, task)
            .lines()
            .any(|line| line.trim_start().starts_with("- [ ]"))
    });
    assert!(
        !all_task_statuses_done || all_task_checklists_checked,
        "{} must not mark ACF-01..ACF-20 or FIX-ACF-01 done/complete while their verification or closeout checklist still has unchecked evidence",
        foundation_path.display()
    );
    let foundation_closed = all_task_statuses_done && all_task_checklists_checked;
    let foundation_title_claims_closed = foundation
        .lines()
        .next()
        .is_some_and(|line| line.contains("已收口"));
    assert!(
        !foundation_title_claims_closed || foundation_closed,
        "{} must not claim the foundation is closed before all ACF task evidence is complete",
        foundation_path.display()
    );

    let gate_claims_released = |line: &str| {
        let still_blocked = line.contains("blocked")
            || line.contains("等待")
            || line.contains("等 ACF-09")
            || line.contains("待 ACF-09");
        !still_blocked
            && (line.contains("已交接")
                || line.contains("前置已满足")
                || line.contains("已由 ACF-09")
                || (line.contains("ACF-09")
                    && line.contains("Lifecycle=done")
                    && line.contains("Acceptance=complete")))
    };

    for (plan, gate) in [
        ("plan-20260902.md", "DEP-ACF-MIRROR"),
        ("plan-20260904.md", "DEP-ACF-MIRROR"),
        ("plan-20260905.md", "DEP-ACF-MIRROR"),
        ("plan-20260911.md", "DEP-ACF-MIRROR"),
        ("plan-20260916.md", "DEP-ACF-CAP"),
    ] {
        let path = plan_dir.join(plan);
        let document = fs::read_to_string(&path).expect("read downstream plan");
        let gate_lines = document
            .lines()
            .filter(|line| line.contains(gate))
            .collect::<Vec<_>>();
        assert!(
            !gate_lines.is_empty(),
            "{} must retain the {gate} dependency record",
            path.display()
        );
        assert!(
            document.contains("ACF-09")
                && document.contains("Lifecycle=done")
                && document.contains("Acceptance=complete"),
            "{} must make downstream release conditional on completed ACF-09, not merely mention it",
            path.display()
        );
        if foundation_closed {
            assert!(
                gate_lines.iter().any(|line| gate_claims_released(line)),
                "{} must record the {gate} handoff only after the foundation is actually closed",
                path.display()
            );
        } else {
            assert!(
                gate_lines
                    .iter()
                    .any(|line| line.contains("blocked") || line.contains("等待")),
                "{} must keep {gate} blocked while ACF-09 closeout evidence is incomplete",
                path.display()
            );
            assert!(
                !gate_lines.iter().any(|line| gate_claims_released(line)),
                "{} must not advertise {gate} as handed off on any record before foundation closeout",
                path.display(),
            );
        }
    }

    // Memory has its deliberately narrower, versioned consumer contract:
    // it may use the ACF-02 early contract only if its own explicit CLI/live
    // tests are present, otherwise it waits for ACF-08.  It must not be made
    // to wait for the provider handoff (ACF-09), which would create an
    // unnecessary reverse dependency.
    let memory_plan_path = plan_dir.join("plan-20260926.md");
    let memory_plan = fs::read_to_string(&memory_plan_path).expect("read Memory plan");
    for required in ["DEP-ACF-DM06", "CTR-ACF-DM06-v1", "ACF-02", "ACF-08"] {
        assert!(
            memory_plan.contains(required),
            "{} must retain its named Agent Capture consumer contract {required}",
            memory_plan_path.display()
        );
    }

    for plan in ["plan-status.md", "plan-long.md"] {
        let path = plan_dir.join(plan);
        let document = fs::read_to_string(&path).expect("read plan index");
        let foundation_rows = document
            .lines()
            .filter(|line| line.contains("[`plan-20260924.md`]"))
            .collect::<Vec<_>>();
        assert!(
            !foundation_rows.is_empty(),
            "{} must contain a Session Capture foundation row",
            path.display()
        );
        assert!(
            foundation_rows.iter().all(|row| row.contains("ACF-09")),
            "{} must retain the ACF-09 handoff in the Session Capture foundation row",
            path.display()
        );
        if foundation_closed {
            assert!(
                foundation_rows.iter().any(|row| row.contains("已收口")),
                "{} must reflect Session Capture foundation closure after all ACF evidence is complete",
                path.display()
            );
        } else {
            assert!(
                foundation_rows.iter().all(|row| !row.contains("已收口")),
                "{} must not report Session Capture as closed before all ACF evidence is complete",
                path.display()
            );
            let premature_handoffs = document
                .lines()
                .filter(|line| {
                    (line.contains("DEP-ACF-MIRROR") || line.contains("DEP-ACF-CAP"))
                        && gate_claims_released(line)
                })
                .collect::<Vec<_>>();
            assert!(
                premature_handoffs.is_empty(),
                "{} must not retain stale ACF-09 handoff claims while the foundation is open:\n{}",
                path.display(),
                premature_handoffs.join("\n")
            );
        }
    }
}

/// Provider adapters own only wire parsing and hook configuration. They must
/// never acquire a database/catalog handle or mutate history/traces refs: the
/// validated runtime/coordinator owns those effects after ingress. Settings
/// modules may use ordinary filesystem APIs to install provider hooks, so this
/// guard deliberately targets persistence and ref-writing dependencies rather
/// than banning all I/O.
#[test]
fn hook_provider_modules_do_not_import_persistence_or_ref_layers() {
    use syn::visit::Visit;

    fn flatten_use(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        let join = |prefix: &str, ident: &dyn std::fmt::Display| {
            if prefix.is_empty() {
                ident.to_string()
            } else {
                format!("{prefix}::{ident}")
            }
        };
        match tree {
            syn::UseTree::Path(path) => flatten_use(&path.tree, &join(prefix, &path.ident), out),
            syn::UseTree::Name(name) if name.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Rename(rename) if rename.ident == "self" => {
                out.push(prefix.to_string());
            }
            syn::UseTree::Name(name) => out.push(join(prefix, &name.ident)),
            syn::UseTree::Rename(rename) => out.push(join(prefix, &rename.ident)),
            syn::UseTree::Glob(_) => out.push(join(prefix, &"*")),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    flatten_use(item, prefix, out);
                }
            }
        }
    }

    fn forbidden_reason(path: &str) -> Option<&'static str> {
        let path = path.strip_prefix("crate::").unwrap_or(path);
        let starts = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}::"));
        if starts("sea_orm") || starts("internal::db") || starts("db") {
            return Some("database/catalog dependency");
        }
        if starts("internal::ai::history")
            || starts("ai::history")
            || path.ends_with("::HistoryManager")
            || path == "HistoryManager"
        {
            return Some("history writer dependency");
        }
        if starts("internal::ai::traces")
            || starts("ai::traces")
            || path.ends_with("::CheckpointCommit")
            || path == "CheckpointCommit"
        {
            return Some("traces/checkpoint dependency");
        }
        if starts("git_internal")
            || starts("internal::protocol::git_client")
            || starts("utils::client_storage")
            || path.ends_with("::ClientStorage")
            || path == "ClientStorage"
            || path.ends_with("::write_git_object")
            || path == "write_git_object"
        {
            return Some("git-ref or object-store dependency");
        }
        None
    }

    fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string().trim() == "test")
        })
    }

    struct ProviderBoundaryGuard {
        violations: Vec<String>,
    }

    impl<'ast> Visit<'ast> for ProviderBoundaryGuard {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if has_cfg_test(&item.attrs) {
                return;
            }
            syn::visit::visit_item_mod(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = forbidden_reason(&path) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
            }
            syn::visit::visit_item_use(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let joined = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(reason) = forbidden_reason(&joined) {
                self.violations.push(format!("path {joined} → {reason}"));
            }
            syn::visit::visit_path(self, path);
        }
    }

    let providers = repo_root().join("src/internal/ai/hooks/providers");
    let mut stack = vec![providers];
    let mut checked = 0usize;
    let mut violations = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).expect("read hook provider directory") {
            let path = entry.expect("provider directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read hook provider source");
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let mut guard = ProviderBoundaryGuard {
                violations: Vec::new(),
            };
            guard.visit_file(&file);
            checked += 1;
            for violation in guard.violations {
                violations.push(format!("{}: {violation}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "hook provider modules must remain parser/configuration-only:\n{}",
        violations.join("\n")
    );
    assert!(
        checked >= 10,
        "expected to scan hook provider sources, got {checked}"
    );
}

/// `agent_for` is total over `AgentKind` and each adapter reports the kind
/// it was registered under; the registry row exists for every kind.
#[test]
fn all_known_agent_kinds_resolve_non_null_adapter() {
    for kind in AgentKind::all() {
        let agent = agent_for(*kind);
        assert_eq!(agent.provider_kind(), *kind);
        let row = registration_for(*kind);
        assert_eq!(row.db_value, kind.as_db_str());
        // The capability introspection default must not panic for any kind.
        let _ = agent.declared_capabilities();
    }
}

/// External `libra-agent-*` binaries never appear in the static roster —
/// registration requires the AG-18 `info`/trust flow, so the static matrix
/// only carries built-in rows and unknown slugs stay quarantined.
#[test]
fn external_agent_info_is_required_for_registration() {
    for row in registry() {
        assert!(
            !row.external_binary,
            "{}: static registry rows must be built-in adapters; external agents \
             register through the AG-18 info/trust flow only",
            row.slug
        );
    }
    assert_eq!(
        lookup_cli_slug("libra-agent-anything"),
        SlugLookup::UnknownQuarantined
    );
}

/// The `agent_session.agent_kind` SQL CHECK constraint, the Rust enum and
/// the tracing/agent.md roster stay in sync.
#[test]
fn agent_kind_enum_sql_check_and_doc_roster_stay_in_sync() {
    // Rust enum → db values.
    let enum_values: BTreeSet<String> = AgentKind::all()
        .iter()
        .map(|kind| kind.as_db_str().to_string())
        .collect();

    // SQL CHECK constraint values from the capture migration.
    let migration =
        fs::read_to_string(repo_root().join("sql/migrations/2026050303_agent_capture.sql"))
            .expect("read agent capture migration");
    let check_block = migration
        .split("`agent_kind`           TEXT NOT NULL CHECK(`agent_kind` IN (")
        .nth(1)
        .and_then(|rest| rest.split("))").next())
        .expect("agent_kind CHECK block present in migration");
    let sql_values: BTreeSet<String> = check_block
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert_eq!(
        sql_values, enum_values,
        "agent_session.agent_kind CHECK constraint drifted from AgentKind::as_db_str"
    );

    // Doc roster (docs/development/tracing/agent.md 第一批支持项目) matches
    // the registry's supported set.
    let agent_doc = fs::read_to_string(repo_root().join("docs/development/tracing/agent.md"))
        .expect("read tracing/agent.md");
    let supported: Vec<&str> = registry()
        .iter()
        .filter(|row| row.supported)
        .map(|row| row.slug)
        .collect();
    assert_eq!(supported, ["claude-code", "codex", "opencode"]);
    for slug in &supported {
        assert!(
            agent_doc.contains(&format!("| `{slug}` |")),
            "tracing/agent.md first-batch roster table must list {slug}"
        );
    }
    // The doc must keep declaring the frozen first-batch roster line.
    assert!(
        agent_doc.contains("`claude-code` / `codex` / `opencode`"),
        "tracing/agent.md must keep the frozen first-batch roster statement"
    );
}

/// W0-03: TUI removal is forbidden until the Web-only completion checklist is
/// both complete and still tied to the runtime source seam. While TUI remains
/// compiled, this test freezes the complete checklist (parity dimensions,
/// A0 inputs, non-Code TUI consumers, and fixed product decisions) so later
/// phases cannot silently omit a required closeout item.
#[test]
fn code_web_only_completion_gate() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    let mut missing = Vec::new();

    if !code_doc.contains("## Web-only completion gate（W0-03）") {
        missing.push("heading:## Web-only completion gate（W0-03）".to_string());
    }
    // AC2: current Web-only direct-turn is explicitly not a completion state.
    if !(code_doc.contains("当前 Web-only direct-turn 不是完成态")
        || code_doc.contains("这不是 Web-only completion"))
    {
        missing.push("AC2:direct-turn-not-complete".to_string());
    }

    let gates = [
        ("GATE-WEB-PLAN", "plan workflow parity"),
        ("GATE-WEB-GOAL", "goal/task parity"),
        ("GATE-WEB-RESUME", "resume parity"),
        ("GATE-WEB-APPROVAL", "approval/cancel parity"),
        ("GATE-WEB-SSE", "SSE gap/backpressure"),
        ("GATE-WEB-CODEX", "Codex normalization"),
        ("GATE-WEB-MCP", "MCP / `code --control stdio` boundary"),
        ("GATE-WEB-DOCS", "docs/compat closeout"),
    ];
    for (gate, phrase) in gates {
        let listed = code_doc.contains(&format!("| [ ] {gate}"))
            || code_doc.contains(&format!("| [x] {gate}"));
        if !listed {
            missing.push(format!("gate:{gate}"));
        }
        if !code_doc.contains(phrase) {
            missing.push(format!("gate-phrase:{phrase}"));
        }
    }
    for decision in [
        "GATE-WEB-DECISION-WEB-ONLY",
        "GATE-WEB-DECISION-BAKE",
        "GATE-WEB-DECISION-STDIO",
        "GATE-WEB-DECISION-SSH",
        "GATE-WEB-DECISION-GRAPH",
    ] {
        if !code_doc.contains(decision) {
            missing.push(format!("decision:{decision}"));
        }
    }
    // AC4: A0-02..A0-11 are completed inputs; the gate must not recreate them.
    if !(code_doc.contains("A0-02..A0-11") && code_doc.contains("不因为本清单而被复制")) {
        missing.push("AC4:A0-inputs-not-copied".to_string());
    }
    // AC5: non-Code TUI consumers stay visible so W5 cannot orphan-delete tui.
    for consumer in [
        "src/command/graph.rs",
        "src/command/agent/graph.rs",
        "TuiControlError",
        "src/internal/ai/agent/format.rs",
    ] {
        if !code_doc.contains(consumer) {
            missing.push(format!("consumer:{consumer}"));
        }
    }
    // Product-decision fixed phrases (compat window / bake / stdio / SSH / graph).
    for phrase in [
        "--web-only",
        "3 patch",
        "MCP transport",
        "SSH",
        "libra graph",
    ] {
        if !code_doc.contains(phrase) {
            missing.push(format!("decision-phrase:{phrase}"));
        }
    }

    assert!(
        missing.is_empty(),
        "Web-only completion gate missing required items: {}",
        missing.join(", ")
    );

    let internal_mod =
        fs::read_to_string(repo_root().join("src/internal/mod.rs")).expect("read internal/mod.rs");
    let tui_still_compiled = internal_mod.contains("pub mod tui;");
    if !tui_still_compiled {
        let incomplete = gates
            .iter()
            .map(|(gate, _)| *gate)
            .filter(|gate| code_doc.contains(&format!("| [ ] {gate}")))
            .collect::<Vec<_>>();
        assert!(
            incomplete.is_empty(),
            "TUI was removed before Web-only completion gates passed: {}",
            incomplete.join(", ")
        );
        assert!(
            !repo_root().join("src/internal/ai/runtime/mod.rs").exists(),
            "RC-23 deleted the AgentRuntime SCC; runtime/mod.rs must not return"
        );
    }
}

/// W5-10: once the internal terminal UI module is retired, neither its direct
/// dependencies nor its production symbol names may return unnoticed.
#[test]
fn terminal_ui_dependencies_and_production_symbols_remain_retired() {
    let manifest = fs::read_to_string(repo_root().join("Cargo.toml")).expect("read Cargo.toml");
    let manifest: toml::Value = manifest.parse().expect("parse Cargo.toml");

    fn manifest_mentions_dependency(value: &toml::Value, dependency: &str) -> bool {
        let Some(table) = value.as_table() else {
            return false;
        };
        table.iter().any(|(key, value)| {
            key == dependency
                || value
                    .as_table()
                    .and_then(|value| value.get("package"))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|package| package == dependency)
                || manifest_mentions_dependency(value, dependency)
        })
    }

    for dependency in ["ratatui", "crossterm"] {
        let present = manifest_mentions_dependency(&manifest, dependency);
        assert!(
            !present,
            "Cargo.toml must not restore the retired direct {dependency} dependency"
        );
    }

    fn visit_source_tree(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read production source directory") {
            let entry = entry.expect("read production source entry");
            let path = entry.path();
            if path.is_dir() {
                visit_source_tree(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let mut sources = Vec::new();
    visit_source_tree(&repo_root().join("src"), &mut sources);
    let library_root = fs::read_to_string(repo_root().join("src/lib.rs")).expect("read lib.rs");
    assert!(
        library_root.contains("supported, patch-compatible embedding API")
            && library_root.contains("`internal` is an implementation detail"),
        "the public internal module must remain documented as an unstable implementation detail",
    );
    for source in sources {
        let contents = fs::read_to_string(&source).expect("read production Rust source");
        for forbidden in ["ratatui", "crossterm", "internal::tui", "Tui"] {
            assert!(
                !contents.contains(forbidden),
                "{} reintroduced retired terminal-UI token {forbidden:?}",
                source.display()
            );
        }
    }
}

/// W5-05: Code runtime behavior must not return to TUI or a Web-private plan
/// workflow state machine. Docs must keep Web as the default surface.
#[test]
fn code_runtime_stays_web_owned_without_tui_or_private_plan_state() {
    assert!(
        !repo_root().join("src/command/code.rs").exists(),
        "RC-23 deleted src/command/code.rs; it must not return"
    );
    assert!(
        !repo_root().join("src/internal/ai/web").exists(),
        "RC-23 deleted src/internal/ai/web; it must not return"
    );

    let code_doc = fs::read_to_string(repo_root().join("docs/commands/code.md"))
        .expect("read docs/commands/code.md");
    let zh_doc = fs::read_to_string(repo_root().join("docs/commands/zh-CN/code.md"))
        .expect("read docs/commands/zh-CN/code.md");
    for (path, body) in [
        ("docs/commands/code.md", &code_doc),
        ("docs/commands/zh-CN/code.md", &zh_doc),
    ] {
        let lowered = body.to_ascii_lowercase();
        assert!(
            !lowered.contains("default mode launches the tui")
                && !lowered.contains("defaults to the tui")
                && !lowered.contains("默认启动 tui"),
            "{path} must not advertise TUI as the current default"
        );
        assert!(
            body.contains("has been removed") || body.contains("已移除"),
            "{path} must document that the public Code CLI is gone"
        );
        assert!(
            !body.contains("Web Code UI"),
            "{path} must not advertise Web Code UI as a current surface"
        );
    }
}

/// W0-01: retain the source-grounded conflict and A0-consumption records
/// needed to prevent a future Code migration from silently recreating an
/// Agent-side queue, trust policy, or artifact store.
#[test]
fn code_runtime_anchor_audit_is_documented() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    for heading in [
        "### C1–C10 契约冲突表（W0-01）",
        "### A0 接口漂移登记表（W0-01）",
    ] {
        assert!(
            code_doc.contains(heading),
            "tracing/code.md must retain {heading}"
        );
    }
    for contract in 1..=10 {
        assert!(
            code_doc.contains(&format!("| C{contract} |")),
            "runtime contract audit is missing C{contract}"
        );
    }
    for artifact in [
        "A0-02 subagent checkpoint",
        "A0-03 stable error emit",
        "A0-04 run admission",
        "A0-05 fix bridge",
        "A0-06 findings artifacts",
        "A0-07 skill projection",
        "A0-08 trust",
        "A0-09 retention",
        "A0-10 cloud tombstone",
        "A0-11 deferred parity",
        "src/command/agent/graph.rs",
    ] {
        assert!(
            code_doc.contains(artifact),
            "runtime anchor audit is missing {artifact}"
        );
    }
}

/// W1-03: Code workflow JSONL stays additive to the existing session stream
/// and must not silently absorb agent-owned checkpoint/finding/capture data.
#[test]
fn code_session_event_boundary_is_documented() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    for required in [
        "### W1-03 Code 会话事件边界",
        ".libra/sessions/{session_id}/events.jsonl",
        "code_workflow",
        "event_id",
        "sequence: u64",
        "IndeterminateSideEffect",
        "A0-02 subagent checkpoint",
        "A0-06 review/investigate findings",
        "external-agent capture retention",
        "A0-10\ncloud tombstone",
        "W1-05",
        "W1-06",
        "### W1-05 Runtime command durability boundary",
        "RuntimeCommandDurability",
        "command_indeterminate_side_effect",
        "sync_data",
    ] {
        assert!(
            code_doc.contains(required),
            "W1-03 session boundary documentation is missing {required:?}"
        );
    }

    let jsonl = fs::read_to_string(repo_root().join("src/internal/ai/session/jsonl.rs"))
        .expect("read session JSONL implementation");
    for required in [
        "CodeWorkflowEventKind",
        "CommandAccepted",
        "TerminalSuccess",
        "TerminalFailure",
        "IndeterminateSideEffect",
        "append_code_workflow",
        "load_code_workflow_replay",
        "admit_code_command",
        "recover_code_command",
    ] {
        assert!(
            jsonl.contains(required),
            "W1-03 JSONL schema is missing {required}"
        );
    }

    assert!(
        !repo_root()
            .join("src/internal/ai/runtime/durability.rs")
            .exists(),
        "RC-23 deleted runtime durability; the file must not return"
    );
}

#[test]
fn recovery_entry_is_hidden_and_content_free() {
    let worker_source = fs::read_to_string(repo_root().join("src/internal/ai/capture/worker.rs"))
        .expect("read capture recovery worker");
    let (_, worker_ast) = parse_rust_source("src/internal/ai/capture/worker.rs");
    let spawn = top_level_function(&worker_ast, "spawn_detached");
    assert_eq!(spawn.sig.ident, "spawn_detached");
    let spawn_start = worker_source
        .find("pub(crate) fn spawn_detached")
        .expect("find worker spawn implementation");
    let spawn_end = worker_source[spawn_start..]
        .find("/// Acquire the per-repository worker lock")
        .map(|offset| spawn_start + offset)
        .expect("find worker lock boundary");
    let child_source = &worker_source[spawn_start..spawn_end];
    let builder_start = worker_source
        .find("fn detached_command(")
        .expect("find detached child builder");
    let forward_start = worker_source
        .find("fn forward_test_hold(")
        .expect("find debug-only test hold forwarding");
    assert!(
        spawn_start < builder_start && builder_start < forward_start && forward_start < spawn_end
    );
    let spawn_source = &worker_source[spawn_start..builder_start];
    let builder_source = &worker_source[builder_start..forward_start];
    assert!(
        spawn_source.contains("detached_command(&executable, repo_root).spawn()"),
        "the hook parent must launch exactly the fixed detached builder"
    );
    assert!(worker_source.contains("__capture-recovery-worker"));
    for required in [
        ".arg(WORKER_ARG)",
        ".current_dir(repo_root)",
        ".env_clear()",
        ".stdin(Stdio::null())",
        ".stdout(Stdio::null())",
        ".stderr(Stdio::null())",
        "setsid()",
    ] {
        assert!(
            builder_source.contains(required),
            "detached recovery child must satisfy {required}"
        );
    }
    for waiting in [".wait()", ".wait_with_output(", ".output()", ".status()"] {
        assert!(
            !child_source.contains(waiting),
            "the hook parent must never wait for its recovery child ({waiting})"
        );
    }
    // The child environment stays empty in release builds: the only
    // re-population is the debug-build integration-test hold forwarding.
    assert!(
        !builder_source.contains(".env(") && !builder_source.contains(".envs("),
        "the detached builder must not forward environment"
    );
    assert!(
        builder_source.contains("#[cfg(debug_assertions)]\n    forward_test_hold(&mut command);"),
        "test hold forwarding must be compiled only into debug builds"
    );
    let debug_only = |attrs: &[syn::Attribute]| {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list)
                    if list.tokens.to_string().trim() == "debug_assertions")
        })
    };
    for seam in ["forward_test_hold", "hold_for_test"] {
        assert!(
            debug_only(&top_level_function(&worker_ast, seam).attrs),
            "{seam} must be compiled only into debug builds"
        );
    }

    let cli_source =
        fs::read_to_string(repo_root().join("src/cli.rs")).expect("read CLI worker dispatch");
    let worker_dispatch = cli_source
        .find("capture::worker::WORKER_ARG")
        .expect("the worker token is recognized by the raw-argv gate");
    let ordinary_dispatch = cli_source
        .find("parse_async_scoped(argv)")
        .expect("normal CLI startup dispatch exists");
    assert!(
        worker_dispatch < ordinary_dispatch,
        "the hidden child must bypass ordinary config/schema/startup recovery"
    );
    assert!(cli_source.contains("argv.len() != 2"));
    assert!(cli_source.contains("capture recovery worker does not accept arguments"));

    let agent_source = fs::read_to_string(repo_root().join("src/command/agent/mod.rs"))
        .expect("read internal worker dispatch");
    assert!(agent_source.contains("capture::worker::try_lock"));
    assert!(agent_source.contains("doctor::run_pending_artifact_worker()"));
    assert!(
        agent_source.contains(
            "#[cfg(debug_assertions)]\n    crate::internal::ai::capture::worker::hold_for_test().await;"
        ),
        "the worker test hold must be compiled only into debug builds"
    );
}

/// The exact source slice one syntax node's span covers (proc-macro2
/// `span-locations`), with its original formatting and plain comments.
fn span_source_text(source: &str, span: proc_macro2::Span) -> &str {
    let lines: Vec<&str> = source.split('\n').collect();
    let offset = |position: proc_macro2::LineColumn| -> usize {
        let line_start: usize = lines[..position.line - 1]
            .iter()
            .map(|line| line.len() + 1)
            .sum();
        let line = lines[position.line - 1];
        line_start
            + line
                .char_indices()
                .nth(position.column)
                .map_or(line.len(), |(index, _)| index)
    };
    &source[offset(span.start())..offset(span.end())]
}

/// Canonical token text of one syntax node: the exact source slice its span
/// covers, re-tokenized so whitespace and plain comments never matter while
/// doc comments (attributes) and every token remain part of the text.
fn span_token_text(source: &str, span: proc_macro2::Span) -> String {
    span_source_text(source, span)
        .parse::<proc_macro2::TokenStream>()
        .expect("re-tokenize a span of parsed Rust source")
        .to_string()
}

/// Identifiers of a token stream, plus the subset that names a member (the
/// identifier directly follows `.` or `::`, i.e. a method or associated call).
fn token_identifiers(
    tokens: proc_macro2::TokenStream,
    names: &mut BTreeSet<String>,
    members: &mut BTreeSet<String>,
) {
    let mut previous_punct: Option<char> = None;
    for token in tokens {
        match token {
            proc_macro2::TokenTree::Group(group) => {
                token_identifiers(group.stream(), names, members);
                previous_punct = None;
            }
            proc_macro2::TokenTree::Ident(ident) => {
                let ident = ident.to_string();
                if matches!(previous_punct, Some('.' | ':')) {
                    members.insert(ident.clone());
                }
                names.insert(ident);
                previous_punct = None;
            }
            proc_macro2::TokenTree::Punct(punct) => previous_punct = Some(punct.as_char()),
            proc_macro2::TokenTree::Literal(_) => previous_punct = None,
        }
    }
}

/// ADR-ACF-10 oracle fingerprint: the oracle function plus the same-file
/// items it uses directly -- free functions it names, methods of same-file
/// inherent impls it calls through `.`/`::`, and the types and expectation
/// constants it names (constants/statics are closed transitively so an
/// expectation table cannot change behind a constant it is built from).
/// Returns the covered item labels and the SHA-256 of their token text.
fn extraction_oracle_fingerprint(relative_path: &str, oracle: &str) -> (Vec<String>, String) {
    use sha2::{Digest, Sha256};
    use syn::spanned::Spanned;

    let source = fs::read_to_string(repo_root().join(relative_path))
        .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
    let file =
        syn::parse_file(&source).unwrap_or_else(|error| panic!("parse {relative_path}: {error}"));
    let oracle_text = span_token_text(&source, top_level_function(&file, oracle).span());
    let mut names = BTreeSet::new();
    let mut members = BTreeSet::new();
    token_identifiers(
        oracle_text
            .parse()
            .expect("re-tokenize the oracle function"),
        &mut names,
        &mut members,
    );
    let mut covered: std::collections::BTreeMap<String, String> = Default::default();
    loop {
        let before = covered.len();
        for item in &file.items {
            let (label, text) = match item {
                syn::Item::Fn(function)
                    if function.sig.ident != oracle
                        && names.contains(&function.sig.ident.to_string()) =>
                {
                    (
                        format!("fn {}", function.sig.ident),
                        span_token_text(&source, function.span()),
                    )
                }
                syn::Item::Const(constant) if names.contains(&constant.ident.to_string()) => (
                    format!("const {}", constant.ident),
                    span_token_text(&source, constant.span()),
                ),
                syn::Item::Static(value) if names.contains(&value.ident.to_string()) => (
                    format!("static {}", value.ident),
                    span_token_text(&source, value.span()),
                ),
                syn::Item::Struct(value) if names.contains(&value.ident.to_string()) => (
                    format!("struct {}", value.ident),
                    span_token_text(&source, value.span()),
                ),
                syn::Item::Enum(value) if names.contains(&value.ident.to_string()) => (
                    format!("enum {}", value.ident),
                    span_token_text(&source, value.span()),
                ),
                syn::Item::Impl(implementation) if implementation.trait_.is_none() => {
                    let self_type = match implementation.self_ty.as_ref() {
                        syn::Type::Path(path) => syn_path_text(&path.path),
                        _ => continue,
                    };
                    for member in &implementation.items {
                        if let syn::ImplItem::Fn(method) = member
                            && members.contains(&method.sig.ident.to_string())
                        {
                            covered.insert(
                                format!("{self_type}::{}", method.sig.ident),
                                span_token_text(&source, method.span()),
                            );
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            // Data items are closed transitively; functions stay direct-only.
            if !label.starts_with("fn ") && !covered.contains_key(&label) {
                let mut nested = BTreeSet::new();
                token_identifiers(
                    text.parse().expect("re-tokenize a covered data item"),
                    &mut nested,
                    &mut BTreeSet::new(),
                );
                names.extend(nested.into_iter().filter(|name| {
                    file.items.iter().any(|candidate| match candidate {
                        syn::Item::Const(constant) => constant.ident == name.as_str(),
                        syn::Item::Static(value) => value.ident == name.as_str(),
                        _ => false,
                    })
                }));
            }
            covered.insert(label, text);
        }
        if covered.len() == before {
            break;
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(format!("fn {oracle}\n{oracle_text}"));
    for (label, text) in &covered {
        hasher.update(format!("\n--\n{label}\n{text}"));
    }
    (
        covered.keys().cloned().collect(),
        hex::encode(hasher.finalize()),
    )
}

/// SHA-256 of the public signature (`vis` + `sig`) of one top-level function.
fn public_signature_fingerprint(relative_path: &str, function: &str) -> String {
    use sha2::{Digest, Sha256};
    use syn::spanned::Spanned;

    let source = fs::read_to_string(repo_root().join(relative_path))
        .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
    let file =
        syn::parse_file(&source).unwrap_or_else(|error| panic!("parse {relative_path}: {error}"));
    let item = top_level_function(&file, function);
    assert!(
        matches!(item.vis, syn::Visibility::Public(_)),
        "{function} must remain a public harness entry point"
    );
    let signature = span_token_text(
        &source,
        item.vis
            .span()
            .join(item.sig.span())
            .expect("join the harness visibility and signature spans"),
    );
    hex::encode(Sha256::digest(signature.as_bytes()))
}

/// ADR-ACF-10 "Oracle freeze" (ACF-17): the three live-runtime extraction
/// oracles, the same-file helpers they use directly, and the public signature
/// of the in-process AgentTraces harness are fingerprinted before the first
/// move. ACF-18/19/20 must not change these constants; a genuine oracle change
/// requires an ADR-ACF-10 revision and a fresh design review first.
#[test]
fn capture_runtime_extraction_oracles_are_frozen() {
    // Digests computed at ACF-17 on the unmodified runtime, before the first
    // move (ADR-ACF-10 "Oracle freeze"); ACF-18/19/20 must not change them.
    const FROZEN_ORACLES: [(&str, &str, &[&str], &str); 3] = [
        (
            "tests/agent_lifecycle_event_test.rs",
            "hook_public_contract_byte_compat_matrix",
            &[
                "HookRepo::envelope",
                "HookRepo::envelope_at",
                "HookRepo::init",
                "HookRepo::run",
                "const CLAUDE_CONTRACT_EVENTS",
                "const CODEX_CONTRACT_EVENTS",
                "const HOOK_CONTRACT_SURFACES",
                "const HOOK_CONTRACT_VERBS",
                "const HOOK_PUBLIC_CONTRACT_MATRIX",
                "const OPENCODE_CONTRACT_EVENTS",
                "fn hook_contract_bytes",
                "fn hook_contract_projection",
                "struct HookRepo",
            ],
            "1abe93f409ff34e94649f545abdb845bb255b2cd3c721031b627dc7a8c5b883a",
        ),
        (
            "tests/agent_hook_crash_test.rs",
            "capture_coordinator_native_replay_matrix",
            &[
                "HookRepo::checkpoint_receipts",
                "HookRepo::checkpoints",
                "HookRepo::coverage_claims",
                "HookRepo::envelope",
                "HookRepo::inflight_marker_count",
                "HookRepo::inflight_markers_for",
                "HookRepo::init",
                "HookRepo::replay_durable_oracle",
                "HookRepo::run",
                "HookRepo::session_ledger",
                "HookRepo::session_show",
                "HookRepo::traces_head",
                "HookRepo::traces_head_if_present",
                "HookRepo::write_claude_transcript",
                "ReplayFault::arm",
                "ReplayFault::disarm",
                "const TURN_COMPLETE",
                "enum ReplayFault",
                "fn describe",
                "struct HookRepo",
                "struct ReplayCase",
                "struct ReplayDurableOracle",
            ],
            "1a32c4659d50b14ae37b1ac9a0d1eb7b8a0934e6c2005c44aed107f7a1beee9f",
        ),
        (
            "src/internal/ai/capture/live_oracle_tests.rs",
            "live_checkpoint_metadata_shape_is_stable",
            &[
                "OracleEnvGuard::set",
                "const LIVE_CHECKPOINT_METADATA_SHAPE",
                "const ORACLE_CLAUDE_TRANSCRIPT",
                "fn oracle_deliver",
                "fn oracle_helper_program",
                "fn oracle_normalize",
                "fn oracle_object_body",
                "fn oracle_repository",
                "fn oracle_tree_blob",
                "struct OracleEnvGuard",
            ],
            "a1bf4daac211dde6204a1840967e234287f912eee8e86ee60515357cd71924e3",
        ),
    ];
    const FROZEN_HARNESS_SIGNATURE: &str =
        "59dcac0ec79268681a04242f71650b028cc5b834e322e97c048b99aff6642244";

    let mut drift = Vec::new();
    for (relative_path, oracle, helpers, digest) in FROZEN_ORACLES {
        let (covered, actual) = extraction_oracle_fingerprint(relative_path, oracle);
        if covered != helpers || actual != digest {
            drift.push(format!(
                "{relative_path}::{oracle}: helpers {covered:?} digest {actual}"
            ));
        }
    }
    let harness = public_signature_fingerprint(
        "src/internal/ai/capture/test_support.rs",
        "ingest_agent_traces_ingress_outcome_for_test",
    );
    if harness != FROZEN_HARNESS_SIGNATURE {
        drift.push(format!(
            "capture::test_support::ingest_agent_traces_ingress_outcome_for_test signature digest {harness}"
        ));
    }
    assert!(
        drift.is_empty(),
        "frozen ACF-17 extraction oracles changed (revise ADR-ACF-10 first):\n{}",
        drift.join("\n")
    );

    // Self-test the fingerprint primitive: formatting and plain comments are
    // not part of the token text, while any token or doc change is.
    let reflowed = "fn probe ( ) { // comment\n    let value = 1 ;\n}";
    let compact = "fn probe() { let value = 1; }";
    let changed = "fn probe() { let value = 2; }";
    let text = |source: &str| {
        source
            .parse::<proc_macro2::TokenStream>()
            .expect("parse probe")
            .to_string()
    };
    assert_eq!(text(reflowed), text(compact));
    assert_ne!(text(compact), text(changed));
}
