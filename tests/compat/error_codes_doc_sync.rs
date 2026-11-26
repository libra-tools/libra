//! `tests/compat/error_codes_doc_sync.rs` — surface contract
//! guaranteeing every `StableErrorCode` enum variant emits a code
//! string that is also documented in `docs/error-codes.md`.
//!
//! Without this guard, adding a new error variant in
//! `src/utils/error.rs` (and wiring it through `as_str()`) would let
//! the user-facing reference silently fall behind, leaving operators
//! and AI agents unable to look up the new code's category / meaning /
//! exit semantics in the canonical doc. The doc is also embedded into
//! the binary via `include_str!` for `libra help error-codes`, so a
//! missing entry there is also a missing entry in the runtime help.
//!
//! The guard parses each `LBR-*` literal from `src/utils/error.rs`
//! (the `as_str()` arm body) and asserts each one appears verbatim in
//! `docs/error-codes.md`. We do not assert the other direction (every
//! doc code maps to an enum variant) because the doc may legitimately
//! describe legacy codes during migration windows.

use std::collections::BTreeSet;

const ERROR_RS: &str = include_str!("../../crates/libra-core/src/utils/error.rs");
const ERROR_DOC: &str = include_str!("../../docs/error-codes.md");

fn collect_code_literals(src: &str) -> BTreeSet<String> {
    let mut codes: BTreeSet<String> = BTreeSet::new();
    // The `as_str()` arms produce string literals shaped `"LBR-FOO-NNN"`.
    // We split by `"` and accept any segment that begins with `LBR-`
    // and contains a `-NNN` suffix. The `assert_eq!` block in the
    // unit tests at the bottom of error.rs also contains the same
    // literals, which is fine — uniqueness in the BTreeSet handles
    // the duplicates.
    for segment in src.split('"') {
        if !segment.starts_with("LBR-") {
            continue;
        }
        // Accept only `LBR-<UPPER>-<digits>` shape; rejects URL-style
        // false positives like `LBR-error-codes`.
        let after_prefix = &segment[4..];
        let Some(dash_idx) = after_prefix.find('-') else {
            continue;
        };
        let (domain, rest) = after_prefix.split_at(dash_idx);
        if !domain.chars().all(|c| c.is_ascii_uppercase()) || domain.is_empty() {
            continue;
        }
        // Skip the leading dash.
        let number = &rest[1..];
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        codes.insert(segment.to_string());
    }
    codes
}

#[test]
fn every_stable_error_code_appears_in_user_facing_doc() {
    let codes = collect_code_literals(ERROR_RS);
    assert!(
        !codes.is_empty(),
        "no LBR-*-NNN literals were found in src/utils/error.rs — has \
         the as_str() arm changed shape?"
    );

    let mut missing: Vec<String> = Vec::new();
    for code in &codes {
        if !ERROR_DOC.contains(code) {
            missing.push(code.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "docs/error-codes.md is out of date with src/utils/error.rs. \
         Add the following code(s) to the 'Exit / Stable / Category' \
         summary table, the matching 'Stable Codes By Category' \
         section, and (if appropriate) a Category narrative block: \
         {missing:?}"
    );
}

/// One `LBR-*` code must map to exactly one `StableErrorCode` variant —
/// duplicated literals in the `as_str()` match would silently give two
/// semantics the same wire code (the `LBR-AGENT-001` near-collision that
/// AG-18/E10 planning caught). Scans only the `code()`/`as_str()` match
/// arm region (`Self::X => "LBR-..."` lines) so pin-test literals don't
/// count as duplicates.
#[test]
fn no_stable_code_literal_maps_to_multiple_variants() {
    use std::collections::BTreeMap;
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for line in ERROR_RS.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("Self::") else {
            continue;
        };
        // Shape: Self::Variant => "LBR-FOO-NNN",
        let Some((variant, tail)) = rest.split_once(" => \"") else {
            continue;
        };
        let Some(code) = tail.split('"').next() else {
            continue;
        };
        if !code.starts_with("LBR-") {
            continue;
        }
        seen.entry(code).or_default().push(variant);
    }
    let duplicates: Vec<String> = seen
        .iter()
        .filter(|(_, variants)| variants.len() > 1)
        .map(|(code, variants)| format!("{code} -> {variants:?}"))
        .collect();
    assert!(
        duplicates.is_empty(),
        "a stable LBR code maps to multiple StableErrorCode variants: {duplicates:?}"
    );
}
