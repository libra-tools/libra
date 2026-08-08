//! plan-20260714 §C.5 (W2): the pseudo-ref surface is DECLARED, not implied.
//!
//! §C.5 requires that when `WorktreePseudoRefs` ships, `COMPATIBILITY.md`'s
//! `rev-parse` row states that the pseudo-ref names are not resolvable — so a
//! Git user who reaches for `rev-parse MERGE_HEAD` mid-conflict finds a
//! contract instead of a silent failure. This guard fails if the service and
//! the declaration ever drift apart in either direction. (A W2-review
//! attempt to wire the names into `rev-parse` was ROLLED BACK against this
//! very contract — §C.5 keeps public resolution deferred and says that if it
//! ever lands, it lands only through the service; the per-worktree isolation
//! itself is pinned by `linked_pseudo_refs_resolve_per_worktree` at the
//! service level.)

use std::process::Command;

/// Every name the service declares, from `src/internal/pseudo_ref.rs`'s own
/// table. Kept here as a literal on purpose: a guard that imported the list
/// from the code under test could not notice the list changing.
const DECLARED: [&str; 6] = [
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "FETCH_HEAD",
];

fn rev_parse_row() -> String {
    let compat = include_str!("../../COMPATIBILITY.md");
    compat
        .lines()
        .find(|line| line.starts_with("| rev-parse | "))
        .expect("COMPATIBILITY.md has a rev-parse row")
        .to_string()
}

#[test]
fn the_rev_parse_row_declares_every_pseudo_ref_name() {
    let row = rev_parse_row();
    for name in DECLARED {
        assert!(
            row.contains(name),
            "COMPATIBILITY.md's rev-parse row must name `{name}` as unresolvable \
             (§C.5): a user who tries it gets no contract otherwise"
        );
    }
    assert!(
        row.contains("not resolvable") || row.contains("NOT accepted"),
        "and it must say they are not resolvable, not merely mention them: {row}"
    );
}

/// EXACTLY the same set, in both directions.
///
/// A containment check cannot see an ADDITION: adding `BISECT_HEAD` to the
/// enum would leave a "does the row mention each of my six names" test green
/// while the compatibility row silently stopped describing the surface. So
/// this reads the service's own declared set out of `PseudoRef::ALL` and
/// compares it to the literal list above, and to the names the row declares.
#[test]
fn the_service_declares_exactly_the_same_names() {
    let source = include_str!("../../src/internal/pseudo_ref.rs");
    let all = source
        .split_once("pub const ALL: [PseudoRef; ")
        .expect("`PseudoRef::ALL` is the declared set")
        .1;
    let (declared_len, rest) = all.split_once(']').expect("ALL's length");
    assert_eq!(
        declared_len
            .trim()
            .parse::<usize>()
            .expect("ALL's length parses"),
        DECLARED.len(),
        "the service declares a different NUMBER of pseudo-refs than \
         COMPATIBILITY.md does; one of them was updated without the other"
    );

    // The variants inside the literal, mapped back through `name()`'s arms.
    let body = rest
        .split_once('[')
        .expect("ALL's literal opens")
        .1
        .split_once("];")
        .expect("ALL's body")
        .0;
    let variants: Vec<&str> = body
        .split(',')
        .filter_map(|entry| entry.trim().strip_prefix("PseudoRef::"))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    assert_eq!(
        variants.len(),
        DECLARED.len(),
        "every entry of `PseudoRef::ALL` must be a `PseudoRef::` variant: {variants:?}"
    );
    for (variant, name) in variants.iter().zip(DECLARED) {
        let arm = format!("Self::{variant} => \"{name}\"");
        assert!(
            source.contains(&arm),
            "`PseudoRef::ALL` entry {variant} must map to `{name}` in the same \
             order the compatibility row lists them (expected arm `{arm}`)"
        );
    }

    // And the ROW's declared set, parsed rather than sampled: rejecting two
    // hand-picked names could not see a third being added.
    let row = rev_parse_row();
    let mut declared_in_row: Vec<String> = Vec::new();
    for token in row.split(|c: char| !(c.is_ascii_uppercase() || c == '_')) {
        // A pseudo-ref name is SCREAMING_SNAKE and ends in `_HEAD`; nothing
        // else in this row has that shape.
        if token.ends_with("_HEAD") && token.len() > "_HEAD".len() {
            let token = token.to_string();
            if !declared_in_row.contains(&token) {
                declared_in_row.push(token);
            }
        }
    }
    let mut expected: Vec<String> = DECLARED.iter().map(|name| name.to_string()).collect();
    expected.sort();
    declared_in_row.sort();
    assert_eq!(
        declared_in_row, expected,
        "the rev-parse row must declare EXACTLY the names the service defines: \
         a name in the row that nothing projects is a promise, and a name the \
         service defines but the row omits is an undeclared surface"
    );
}

/// The declaration is only true if `rev-parse` really refuses these names.
/// A row that says "not resolvable" while the parser quietly accepts one would
/// be worse than no row at all.
#[test]
fn rev_parse_refuses_the_declared_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    let libra = env!("CARGO_BIN_EXE_libra");
    let init = Command::new(libra)
        .args(["init", "--vault=false", "-q"])
        .current_dir(temp.path())
        .output()
        .expect("init runs");
    assert!(init.status.success(), "init: {init:?}");

    for name in DECLARED {
        let out = Command::new(libra)
            .args(["rev-parse", name])
            .current_dir(temp.path())
            .output()
            .expect("rev-parse runs");
        assert!(
            !out.status.success(),
            "`rev-parse {name}` must fail while the compatibility row calls it \
             unresolvable; it printed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
