//! Registration gate for the wave-0 status test module (plan-20260714 §B.9).
//!
//! Guards against two silent-drop failure modes:
//! 1. `tests/command/status_wave0_test.rs` exists but is not wired into
//!    `tests/command/mod.rs` (CI would silently skip every wave-0 test).
//! 2. The canonical manifest (`STATUS_WAVE0_TESTS`) drifts from the module
//!    contents in either direction.

use std::collections::HashSet;

#[path = "status_wave0_manifest.rs"]
mod status_wave0_manifest;

use status_wave0_manifest::STATUS_WAVE0_TESTS;

const MODULE_PREFIX: &str = "command::status_wave0::";

fn listed_command_tests() -> HashSet<String> {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["test", "--test", "command_test", "--", "--list"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .output()
        .expect("run `cargo test --test command_test -- --list`");

    assert!(
        output.status.success(),
        "`cargo test --test command_test -- --list` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("test list output should be utf-8")
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn status_wave0_manifest_matches_registered_tests() {
    let actual: HashSet<String> = listed_command_tests()
        .into_iter()
        // Prefix filter (not substring) so an unrelated module containing
        // "status_wave0" in a test name cannot satisfy the gate.
        .filter(|name| name.starts_with(MODULE_PREFIX))
        .collect();

    let expected: HashSet<String> = STATUS_WAVE0_TESTS
        .iter()
        .map(|name| format!("{MODULE_PREFIX}{name}"))
        .collect();

    assert_eq!(
        STATUS_WAVE0_TESTS.len(),
        expected.len(),
        "STATUS_WAVE0_TESTS contains duplicate names"
    );
    assert!(
        !expected.is_empty(),
        "STATUS_WAVE0_TESTS must not be empty — the wave-0 module would be silently dropped"
    );
    assert_eq!(
        expected, actual,
        "STATUS_WAVE0_TESTS and tests/command/status_wave0_test.rs drifted; \
         update tests/compat/status_wave0_manifest.rs together with the module"
    );
}

#[test]
fn status_wave0_manifest_is_strictly_sorted() {
    assert!(
        STATUS_WAVE0_TESTS.windows(2).all(|w| w[0] < w[1]),
        "STATUS_WAVE0_TESTS must be strictly alphabetically sorted with no duplicates"
    );
}
