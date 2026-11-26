//! Defensive guard for the v0.17.243 / v0.17.253 production unwrap audit
//! in `src/utils/client_storage.rs`.
//!
//! The dedicated storage runtime, revision-suffix regex literal, regex
//! capture-group projection, and block_on_storage mpsc recv were converted
//! to `.expect("...")` with diagnostic messages identifying the broken
//! contract (storage runtime build failure, regex compile failure,
//! spawned task panic). This test scans the file and fails the build if
//! a contributor reintroduces a bare `.unwrap()` outside any inline
//! tests module.
//!
//! Mirrors the lfs_client / config / head / util guards. Cargo `[[test]]`
//! binaries are separate compilation units, so the scanning code is
//! duplicated by design.

use std::{fs, path::PathBuf};

const TARGET_FILE: &str = "crates/libra-core/src/utils/client_storage.rs";

#[test]
fn client_storage_production_has_no_bare_unwrap_calls() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(TARGET_FILE);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read '{}': {err}", path.display()));

    let mut in_tests_module = false;
    let mut offenders = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_number = idx + 1;
        let trimmed = raw_line.trim_start();

        // Once we cross into the inline tests module, stop scanning;
        // tests legitimately use .unwrap().
        if trimmed.starts_with("#[cfg(test)]") {
            in_tests_module = true;
        }
        if in_tests_module {
            continue;
        }

        // Skip rustdoc and ordinary comments.
        if trimmed.starts_with("//") {
            continue;
        }

        if trimmed.contains(".unwrap()") {
            offenders.push(format!(
                "{TARGET_FILE}:{line_number} reintroduces bare `.unwrap()` in production code: \
                 use `.expect(\"INVARIANT: ...\")` instead (see CLAUDE.md error handling rules)"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "production code in {TARGET_FILE} must use INVARIANT-documented `.expect(\"...\")` \
         instead of bare `.unwrap()`:\n{}",
        offenders.join("\n"),
    );
}
