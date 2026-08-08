//! Registration gate for the wave-0 status test module (plan-20260714 §B.9).
//!
//! Guards against two silent-drop failure modes:
//! 1. `tests/command/status_wave0_test.rs` exists but is not (or no longer
//!    effectively) wired into the `command_test` binary — the whole chain
//!    `tests/command_test.rs` → `mod command;` → `tests/command/mod.rs` →
//!    `mod status_wave0;` is checked, so CI cannot silently skip every
//!    wave-0 test.
//! 2. The canonical manifest (`STATUS_WAVE0_TESTS`) drifts from the module
//!    contents in either direction.
//!
//! This target must not spawn `cargo test --test command_test -- --list` to
//! discover registrations: Cargo holds the target-directory lock while this
//! test runs, so that child Cargo process waits on its parent indefinitely.
//! Instead the files are parsed with `syn`, which strips comments and
//! whitespace like the compiler does, so a commented-out registration or
//! test cannot satisfy the gate; registrations must also be out-of-line and
//! carry no attribute besides the expected `#[path]`, so a
//! `#[cfg(any())]`-disabled item syn still sees cannot pass either. Any
//! shape the collector does not understand — `cfg_attr`, unknown
//! attributes, file-level `#![cfg]`, non-`cfg(unix)` predicates on test
//! functions, item-position macros, nested modules, unmodeled item kinds —
//! panics instead of being silently skipped: fail closed, never fail open.

use std::{collections::HashSet, fs, path::Path};

use syn::{Item, Lit, Meta};

#[path = "status_wave0_manifest.rs"]
mod status_wave0_manifest;

use status_wave0_manifest::{STATUS_WAVE0_TESTS, STATUS_WAVE0_TESTS_UNIX_ONLY};

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Attributes that may appear on a wave-0 module function without hiding a
/// test from the collector. Everything else — `cfg_attr` (can expand to
/// `#[test]`), `rstest`, `test_case`, custom proc-macro attributes —
/// panics, because the collector cannot prove such a function does not
/// become a test.
const BENIGN_FN_ATTRIBUTES: &[&str] = &[
    "cfg",
    "serial",
    "ignore",
    "allow",
    "expect",
    "deny",
    "warn",
    "doc",
    "should_panic",
];

/// File-level (inner) attributes that cannot disable or rewrite a file.
/// `#![cfg(...)]`/`#![cfg_attr(...)]` would compile the whole file away
/// while source collection still counts every declaration, so anything
/// outside this list panics.
const BENIGN_INNER_ATTRIBUTES: &[&str] = &["doc", "allow", "expect", "deny", "warn"];

fn attribute_name(attribute: &syn::Attribute) -> String {
    attribute
        .path()
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn assert_benign_file_attributes(file: &syn::File, which: &str) {
    for attribute in &file.attrs {
        let name = attribute_name(attribute);
        if !BENIGN_INNER_ATTRIBUTES.contains(&name.as_str()) {
            panic!(
                "{which} carries file-level attribute `#![{name}]`, which could disable or \
                 rewrite the whole file while source collection still counts its contents; \
                 extend the gate first"
            );
        }
    }
}

/// The declarations collected from the wave-0 module source.
struct DeclaredTests {
    all: HashSet<String>,
    /// Tests carrying `#[cfg(unix)]` — must equal the manifest's
    /// `STATUS_WAVE0_TESTS_UNIX_ONLY` platform inventory exactly.
    unix_gated: HashSet<String>,
}

/// Return the `#[test]`/`#[tokio::test]` functions declared by the Wave-0
/// module source, failing closed on any shape that could generate, hide,
/// or platform-disable a test another way.
fn declared_wave0_tests(source: &str) -> DeclaredTests {
    let file = syn::parse_file(source).expect("tests/command/status_wave0_test.rs must parse");
    assert_benign_file_attributes(&file, "the wave-0 module");
    let mut declared = DeclaredTests {
        all: HashSet::new(),
        unix_gated: HashSet::new(),
    };
    collect_test_fns(&file.items, &mut declared);
    declared
}

fn collect_test_fns(items: &[Item], declared: &mut DeclaredTests) {
    for item in items {
        match item {
            Item::Fn(function) => {
                let mut is_test = false;
                let mut cfg_attributes = Vec::new();
                for attribute in &function.attrs {
                    let name = attribute_name(attribute);
                    if name == "test" || name == "tokio::test" {
                        is_test = true;
                    } else if name == "cfg" {
                        cfg_attributes.push(attribute);
                    } else if !BENIGN_FN_ATTRIBUTES.contains(&name.as_str()) {
                        panic!(
                            "function `{}` carries attribute `#[{name}]`, which the wave-0 \
                             collector cannot prove harmless; extend the gate alongside the \
                             module instead of letting a test-generating shape pass silently",
                            function.sig.ident
                        );
                    }
                }
                if !is_test {
                    // Helpers may use whatever `cfg` they need: their
                    // absence breaks compilation loudly, never silently.
                    continue;
                }
                let name = function.sig.ident.to_string();
                // The only platform predicate the manifest models is
                // `cfg(unix)`. Anything else on a TEST — `cfg(any())`,
                // `cfg(windows)`, feature gates — could remove the test on
                // every CI host while the manifest still lists it.
                let mut unix_gated = false;
                for attribute in &cfg_attributes {
                    let Meta::List(list) = &attribute.meta else {
                        panic!(
                            "test `{name}` carries a cfg predicate the gate cannot read; \
                             only #[cfg(unix)] is modeled — extend the gate first"
                        );
                    };
                    if list.tokens.to_string() != "unix" {
                        panic!(
                            "test `{name}` carries cfg predicate `{}`; only #[cfg(unix)] is \
                             modeled by the manifest — a disabled or unknown predicate would \
                             drop the test while the gate still counts it",
                            list.tokens
                        );
                    }
                    unix_gated = true;
                }
                if unix_gated {
                    declared.unix_gated.insert(name.clone());
                }
                assert!(
                    declared.all.insert(name.clone()),
                    "Wave-0 source declares duplicate test function `{name}`"
                );
            }
            // A nested module would run its tests under a nested path that
            // the flat manifest cannot express; an item-position macro can
            // expand to arbitrary tests. Both fail closed.
            Item::Mod(module) => panic!(
                "wave-0 module declares nested module `{}`; the flat manifest cannot \
                 represent its tests — extend the gate first",
                module.ident
            ),
            Item::Macro(_) | Item::Verbatim(_) => {
                panic!(
                    "wave-0 module contains an item-position macro or unparsed item; \
                     the collector cannot prove it declares no tests — extend the gate first"
                )
            }
            Item::Use(use_item) => {
                for attribute in &use_item.attrs {
                    let name = attribute_name(attribute);
                    if name != "cfg" && !BENIGN_INNER_ATTRIBUTES.contains(&name.as_str()) {
                        panic!(
                            "use item carries attribute `#[{name}]`, which the wave-0 \
                             collector cannot prove harmless — extend the gate first"
                        );
                    }
                }
            }
            // A struct/impl/const/… could carry an attribute macro that
            // expands to tests; the module today is only `use` items and
            // functions, so anything else fails closed until the gate is
            // taught about it.
            _ => panic!(
                "wave-0 module contains an item kind the collector does not model; an \
                 attribute macro on it could generate tests — extend the gate first"
            ),
        }
    }
}

/// Assert `tests/command_test.rs` (the `command_test` binary root) declares
/// an unadorned, out-of-line `mod command;` and carries no file-disabling
/// attribute — otherwise the whole command tree, wave-0 included, silently
/// leaves CI.
fn assert_harness_registers_command_module(command_test_source: &str) {
    let file = syn::parse_file(command_test_source).expect("tests/command_test.rs must parse");
    assert_benign_file_attributes(&file, "tests/command_test.rs");
    let registered = file.items.iter().any(|item| {
        let Item::Mod(module) = item else {
            return false;
        };
        module.ident == "command" && module.content.is_none() && module.attrs.is_empty()
    });
    assert!(
        registered,
        "tests/command_test.rs must declare an unadorned out-of-line `mod command;` \
         (an attribute could disable it and silently drop the whole command tree)"
    );
}

/// Assert `tests/command/mod.rs` really registers the wave-0 module: an
/// out-of-line `mod status_wave0;` whose ONLY attribute is
/// `#[path = "status_wave0_test.rs"]`, as live code. Syn strips comments,
/// so a commented-out registration cannot pass; requiring exactly that one
/// attribute rejects a `#[cfg(any())]`-disabled registration that syn
/// would still see but rustc would compile away; requiring the out-of-line
/// form rejects an inline decoy module.
fn assert_module_registered(command_mod_source: &str) {
    let file = syn::parse_file(command_mod_source).expect("tests/command/mod.rs must parse");
    assert_benign_file_attributes(&file, "tests/command/mod.rs");
    let registered = file.items.iter().any(|item| {
        let Item::Mod(module) = item else {
            return false;
        };
        if module.ident != "status_wave0" || module.content.is_some() {
            return false;
        }
        let [attribute] = module.attrs.as_slice() else {
            return false;
        };
        let Meta::NameValue(name_value) = &attribute.meta else {
            return false;
        };
        if !name_value.path.is_ident("path") {
            return false;
        }
        let syn::Expr::Lit(literal) = &name_value.value else {
            return false;
        };
        let Lit::Str(path_literal) = &literal.lit else {
            return false;
        };
        path_literal.value() == "status_wave0_test.rs"
    });
    assert!(
        registered,
        "tests/command/status_wave0_test.rs must be registered by tests/command/mod.rs via \
         an out-of-line `#[path = \"status_wave0_test.rs\"] mod status_wave0;` carrying no \
         other attribute (a cfg-disabled registration would silently drop the module)"
    );
}

#[test]
fn status_wave0_manifest_matches_registered_tests() {
    assert_harness_registers_command_module(&read("tests/command_test.rs"));
    assert_module_registered(&read("tests/command/mod.rs"));
    let actual = declared_wave0_tests(&read("tests/command/status_wave0_test.rs"));

    let manifest: HashSet<&str> = STATUS_WAVE0_TESTS.iter().copied().collect();
    assert_eq!(
        STATUS_WAVE0_TESTS.len(),
        manifest.len(),
        "STATUS_WAVE0_TESTS contains duplicate names"
    );

    let unix_only: HashSet<&str> = STATUS_WAVE0_TESTS_UNIX_ONLY.iter().copied().collect();
    assert_eq!(
        STATUS_WAVE0_TESTS_UNIX_ONLY.len(),
        unix_only.len(),
        "STATUS_WAVE0_TESTS_UNIX_ONLY contains duplicate names"
    );
    assert!(
        unix_only.is_subset(&manifest),
        "STATUS_WAVE0_TESTS_UNIX_ONLY must be a subset of STATUS_WAVE0_TESTS"
    );

    // Source parsing is `cfg`-independent: both sides are the full set on
    // every platform, including `#[cfg(unix)]` functions that a Windows
    // binary intentionally does not compile.
    let expected: HashSet<String> = STATUS_WAVE0_TESTS
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    assert!(
        !expected.is_empty(),
        "STATUS_WAVE0_TESTS must not be empty — the wave-0 module would be silently dropped"
    );
    assert_eq!(
        expected, actual.all,
        "STATUS_WAVE0_TESTS and tests/command/status_wave0_test.rs drifted; \
         update tests/compat/status_wave0_manifest.rs together with the module"
    );

    // And the platform inventory is exact in both directions: a
    // `#[cfg(unix)]` test missing from the inventory (or vice versa) means
    // some platform's expected set is wrong.
    let expected_unix: HashSet<String> = STATUS_WAVE0_TESTS_UNIX_ONLY
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        expected_unix, actual.unix_gated,
        "STATUS_WAVE0_TESTS_UNIX_ONLY and the module's #[cfg(unix)] tests drifted"
    );
}

#[test]
fn status_wave0_manifest_is_strictly_sorted() {
    assert!(
        STATUS_WAVE0_TESTS.windows(2).all(|w| w[0] < w[1]),
        "STATUS_WAVE0_TESTS must be strictly alphabetically sorted with no duplicates"
    );
    assert!(
        STATUS_WAVE0_TESTS_UNIX_ONLY.windows(2).all(|w| w[0] < w[1]),
        "STATUS_WAVE0_TESTS_UNIX_ONLY must be strictly alphabetically sorted with no duplicates"
    );
}

// ── Adversarial pins: the collector fails closed on every escape shape ──────

#[test]
fn collector_ignores_block_commented_tests() {
    let declared = declared_wave0_tests(
        "/*\n#[test]\nfn commented_out() {}\n*/\n#[test]\nfn real_test() {}\n",
    );
    assert_eq!(
        declared.all,
        HashSet::from(["real_test".to_string()]),
        "a commented-out test must not be collected"
    );
}

#[test]
fn collector_accepts_interleaved_benign_attributes() {
    let declared = declared_wave0_tests(
        "#[cfg(unix)]\n#[test]\n#[serial]\n#[ignore]\nfn gated() {}\n\
         #[tokio::test(flavor = \"multi_thread\")]\nasync fn async_case() {}\n\
         #[cfg(any())]\nfn helper_with_odd_cfg() {}\n",
    );
    assert_eq!(
        declared.all,
        HashSet::from(["gated".to_string(), "async_case".to_string()]),
        "cfg/serial/ignore riders and tokio flavors are collected; helpers are not"
    );
    assert_eq!(
        declared.unix_gated,
        HashSet::from(["gated".to_string()]),
        "the unix-gated inventory tracks exactly the #[cfg(unix)] tests"
    );
}

#[test]
#[should_panic(expected = "cfg_attr")]
fn collector_rejects_cfg_attr_test_generation() {
    declared_wave0_tests("#[cfg_attr(unix, test)]\nfn sneaky() {}\n");
}

#[test]
#[should_panic(expected = "rstest")]
fn collector_rejects_unknown_test_framework_attributes() {
    declared_wave0_tests("#[rstest]\nfn parameterized() {}\n");
}

#[test]
#[should_panic(expected = "cfg predicate")]
fn collector_rejects_cfg_disabled_test_functions() {
    declared_wave0_tests("#[cfg(any())]\n#[test]\nfn hidden_everywhere() {}\n");
}

#[test]
#[should_panic(expected = "cfg predicate")]
fn collector_rejects_non_unix_platform_gates_on_tests() {
    declared_wave0_tests("#[cfg(windows)]\n#[test]\nfn windows_only() {}\n");
}

#[test]
#[should_panic(expected = "item-position macro")]
fn collector_rejects_item_position_macros() {
    declared_wave0_tests("generate_tests! { alpha, beta }\n");
}

#[test]
#[should_panic(expected = "nested module")]
fn collector_rejects_nested_modules() {
    declared_wave0_tests("mod inner {\n    #[test]\n    fn hidden() {}\n}\n");
}

#[test]
#[should_panic(expected = "file-level attribute")]
fn collector_rejects_file_level_inner_attributes() {
    declared_wave0_tests("#![cfg(any())]\n#[test]\nfn all_disabled() {}\n");
}

#[test]
#[should_panic(expected = "does not model")]
fn collector_rejects_unmodeled_item_kinds() {
    declared_wave0_tests("#[some_attribute_macro]\nstruct CouldGenerateTests;\n");
}

#[test]
#[should_panic(expected = "must be registered")]
fn registration_check_rejects_commented_out_registration() {
    assert_module_registered("// #[path = \"status_wave0_test.rs\"]\n// mod status_wave0;\n");
}

#[test]
#[should_panic(expected = "must be registered")]
fn registration_check_rejects_cfg_disabled_registration() {
    assert_module_registered(
        "#[cfg(any())]\n#[path = \"status_wave0_test.rs\"]\nmod status_wave0;\n",
    );
}

#[test]
#[should_panic(expected = "must be registered")]
fn registration_check_rejects_inline_registration() {
    assert_module_registered("#[path = \"status_wave0_test.rs\"]\nmod status_wave0 {}\n");
}

#[test]
#[should_panic(expected = "file-level attribute")]
fn registration_check_rejects_cfg_disabled_module_file() {
    assert_module_registered(
        "#![cfg(any())]\n#[path = \"status_wave0_test.rs\"]\nmod status_wave0;\n",
    );
}

#[test]
fn registration_check_accepts_live_registration() {
    assert_module_registered("#[path = \"status_wave0_test.rs\"]\nmod status_wave0;\n");
}

#[test]
#[should_panic(expected = "unadorned out-of-line")]
fn harness_check_rejects_cfg_disabled_command_module() {
    assert_harness_registers_command_module("#[cfg(any())]\nmod command;\n");
}

#[test]
#[should_panic(expected = "file-level attribute")]
fn harness_check_rejects_file_level_cfg() {
    assert_harness_registers_command_module("#![cfg(any())]\nmod command;\n");
}

#[test]
fn harness_check_accepts_live_declaration() {
    assert_harness_registers_command_module("//! docs\nmod command;\nmod other_test;\n");
}
