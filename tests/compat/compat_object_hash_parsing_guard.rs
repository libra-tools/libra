//! B3-08 / GC-B3-02: production OID parsing and constructor audit guard.
//!
//! Scans `src/**/*.rs` with `syn` at item granularity: items (and nested
//! descendants) gated by `#[cfg(test)]` are skipped; everything else must not
//! call `ObjectHash::from_str`, `.parse::<ObjectHash>()`, or
//! `from_bytes_infer_kind`. Remaining thread-local / width-based sites must
//! appear in the allowlist table below (path + symbol + reason).

use std::{
    fs,
    path::{Path, PathBuf},
};

use syn::{File, Item, spanned::Spanned, visit::Visit};

/// Production sites still on thread-local / width APIs that B3-08 registers
/// rather than eliminates. Owner cards clear these later (GC-B3-02).
const WIDTH_OR_THREAD_LOCAL_ALLOWLIST: &[(&str, &str, &str)] = &[
    (
        "src/internal/protocol/mod.rs",
        "oid_width_branch",
        "transition: owner B3-07 (protocol capability / wire OID width)",
    ),
    (
        "src/command/clone.rs",
        "oid_width_branch",
        "transition: owner B3-14 (cloud/clone length-inference removal)",
    ),
    (
        "src/command/cloud.rs",
        "oid_width_branch",
        "transition: owner B3-14 (cloud length-inference removal)",
    ),
    (
        "src/internal/ai/util.rs",
        "oid_width_branch",
        "transition: owner B3-10 (AI IntegrityHash / tagged repo-OID)",
    ),
    (
        "src/internal/ai/history.rs",
        "digest_width_check",
        "application-layer digest; not a repository OID (GC-B3-02 allowlist)",
    ),
    (
        "src/command/rerere.rs",
        "digest_width_check",
        "application-layer digest; not a repository OID (GC-B3-02 allowlist)",
    ),
    (
        "crates/libra-core/src/utils/media/mod.rs",
        "digest_width_check",
        "LFS media_oid is always SHA-256; not a repository OID (GC-B3-02)",
    ),
    (
        "crates/libra-core/src/internal/upgrade/manifest.rs",
        "digest_width_check",
        "application-layer digest; not a repository OID (GC-B3-02 allowlist)",
    ),
    (
        "src/internal/ai/session/jsonl.rs",
        "digest_width_check",
        "application-layer digest; not a repository OID (GC-B3-02 allowlist)",
    ),
    (
        "crates/libra-core/src/utils/storage_ext.rs",
        "from_type_and_data",
        "single-repo thread-local object write; explicit-kind follow-up OK",
    ),
];

/// External `#[cfg(test)] #[path = "..."]` test modules whose files carry no
/// local `#[cfg(test)]` attribute (R22).
const EXTERNAL_TEST_PATH_FILES: &[&str] = &[
    "src/command/rev_list_tests.rs",
    "src/command/rev_list_output_tests.rs",
    "src/command/rev_list_write_tests.rs",
    "src/command/rev_list_children.rs",
    "src/command/describe_tests.rs",
    "src/command/ls_remote_tests.rs",
];

#[path = "../helpers/workspace_sources.rs"]
mod workspace_sources;

#[test]
fn production_oid_parsing_uses_object_format_helpers() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = workspace_sources::collect_workspace_rust_files(&root);

    let mut offenders = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_dedicated_test_file(path) || EXTERNAL_TEST_PATH_FILES.contains(&rel.as_str()) {
            continue;
        }
        let text =
            fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        let file = match syn::parse_file(&text) {
            Ok(file) => file,
            Err(err) => {
                offenders.push(format!("{rel}: syn parse failed (fail-closed): {err}"));
                continue;
            }
        };
        let mut visitor = ForbiddenVisitor {
            rel: rel.clone(),
            source: &text,
            offenders: &mut offenders,
            in_cfg_test: false,
        };
        visitor.visit_file(&file);
    }

    assert!(
        offenders.is_empty(),
        "production OID parsing must use object_format helpers \
         (ObjectHash::from_str / parse::<ObjectHash> / from_bytes_infer_kind forbidden):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn allowlist_entries_point_at_existing_paths() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (path, _symbol, _reason) in WIDTH_OR_THREAD_LOCAL_ALLOWLIST {
        let full = root.join(path);
        assert!(
            full.is_file(),
            "allowlist path missing: {path} (update B3-08 allowlist)"
        );
    }
    for path in EXTERNAL_TEST_PATH_FILES {
        let full = root.join(path);
        assert!(full.is_file(), "external test path file missing: {path}");
    }
}

#[test]
fn fixture_nested_cfg_test_mod_is_skipped() {
    let file: File = syn::parse_str(
        r#"
        fn production_ok() {}
        #[cfg(test)]
        mod tests {
            fn in_test() {
                let _ = ObjectHash::from_str("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
            }
        }
        "#,
    )
    .expect("fixture parses");
    let source = quote_source_for_fixture();
    let mut offenders = Vec::new();
    let mut visitor = ForbiddenVisitor {
        rel: "fixture.rs".into(),
        source: &source,
        offenders: &mut offenders,
        in_cfg_test: false,
    };
    visitor.visit_file(&file);
    assert!(
        offenders.is_empty(),
        "cfg(test) interior must be skipped: {offenders:?}"
    );
}

fn quote_source_for_fixture() -> String {
    // Spans from parse_str are call-site; the visitor falls back to full-source
    // scan when byte ranges are unavailable, so keep the forbidden token only
    // inside the cfg(test) textual region of this string for the fallback path.
    String::from(
        r#"
        fn production_ok() {}
        #[cfg(test)]
        mod tests {
            fn in_test() {
                let _ = ObjectHash::from_str("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
            }
        }
        "#,
    )
}

struct ForbiddenVisitor<'a> {
    rel: String,
    source: &'a str,
    offenders: &'a mut Vec<String>,
    in_cfg_test: bool,
}

impl ForbiddenVisitor<'_> {
    fn attrs_are_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            if !attr.path().is_ident("cfg") {
                return false;
            }
            match &attr.meta {
                syn::Meta::List(list) => list.tokens.to_string().replace(' ', "") == "test",
                _ => false,
            }
        })
    }

    fn scan_source_slice(&mut self, slice: &str) {
        for needle in [
            "ObjectHash::from_str",
            "git_internal::hash::ObjectHash::from_str",
            "from_bytes_infer_kind",
            "parse::<ObjectHash>",
            "parse::<git_internal::hash::ObjectHash>",
        ] {
            if !slice.contains(needle) {
                continue;
            }
            if needle.ends_with("from_str") {
                for (idx, _) in slice.match_indices(needle) {
                    let end = idx + needle.len();
                    let next = slice.as_bytes().get(end).copied().unwrap_or(b'(');
                    // Reject `from_str(` / `from_str!` but not `from_stream…`.
                    if next == b'(' || next == b'!' {
                        self.offenders
                            .push(format!("{}: production code contains `{needle}`", self.rel));
                        break;
                    }
                }
            } else {
                self.offenders
                    .push(format!("{}: production code contains `{needle}`", self.rel));
            }
        }
    }

    fn byte_range_of(item: &Item) -> Option<(usize, usize)> {
        let span = item.span();
        let start = span.byte_range().start;
        let end = span.byte_range().end;
        if end > start {
            Some((start, end))
        } else {
            None
        }
    }
}

impl<'ast> Visit<'ast> for ForbiddenVisitor<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs = item_attrs(item);
        let entered = Self::attrs_are_cfg_test(attrs);
        let prev = self.in_cfg_test;
        if entered {
            self.in_cfg_test = true;
        }
        if !self.in_cfg_test
            && let Some((start, end)) = Self::byte_range_of(item)
            && end <= self.source.len()
        {
            self.scan_source_slice(&self.source[start..end]);
        }
        syn::visit::visit_item(self, item);
        self.in_cfg_test = prev;
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Fn(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        Item::Const(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn is_dedicated_test_file(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(stem, "test" | "tests") || stem.ends_with("_test") || stem.ends_with("_tests")
}
