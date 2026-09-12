//! plan-20260714 §C.5 (W2): the pseudo-ref surface is DECLARED, not implied.
//!
//! The pseudo-ref surface is DECLARED, not implied. `AUTO_MERGE` is the one
//! exception: while a conflicted merge state exists, the public opt-in
//! consumers may resolve its automatic result tree; the six historical names
//! remain unavailable to public object resolution.

use std::process::Command;

/// Every name the service declares, from `src/internal/pseudo_ref.rs`'s own
/// table. Kept here as a literal on purpose: a guard that imported the list
/// from the code under test could not notice the list changing.
const DECLARED: [&str; 7] = [
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "FETCH_HEAD",
    "AUTO_MERGE",
];

const REJECTED: [&str; 6] = [
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "FETCH_HEAD",
];

fn rev_parse_rows() -> String {
    let compat = include_str!("../../COMPATIBILITY.md");
    compat
        .lines()
        .filter(|line| line.starts_with("| rev-parse "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn documented_pseudo_ref_set() -> Vec<String> {
    let row = rev_parse_rows();
    let (_, listed) = row
        .split_once("The complete pseudo-ref set is ")
        .expect("COMPATIBILITY.md declares the complete pseudo-ref set");
    let (listed, _) = listed
        .split_once(". `AUTO_MERGE` alone")
        .expect("pseudo-ref set ends before the AUTO_MERGE contract");
    listed
        .split('`')
        .enumerate()
        .filter_map(|(index, token)| (index % 2 == 1).then_some(token.to_string()))
        .collect()
}

#[test]
fn the_rev_parse_row_declares_every_pseudo_ref_name() {
    let row = rev_parse_rows();
    for name in DECLARED {
        assert!(
            row.contains(name),
            "COMPATIBILITY.md's rev-parse row must name `{name}` as unresolvable \
             (§C.5): a user who tries it gets no contract otherwise"
        );
    }
    assert!(row.contains("not resolvable") || row.contains("NOT accepted"));
    assert!(row.contains("AUTO_MERGE"));
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

    // And the documented set, parsed from the explicit public list rather
    // than inferred from the historical `_HEAD` suffix (which AUTO_MERGE does
    // not carry). This catches both an undocumented service addition and a
    // stale name left in the compatibility contract.
    let mut declared_in_row = documented_pseudo_ref_set();
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

    for name in REJECTED {
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
