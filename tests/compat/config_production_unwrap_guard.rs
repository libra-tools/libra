//! Defensive guard for the v0.17.246-248 production unwrap audit in
//! `src/internal/config.rs`.
//!
//! Every legacy `Config::*_with_conn` call site in that file has been
//! migrated to use `.expect("legacy Config::...")` with an INVARIANT
//! comment that names the broken contract and points readers at the
//! Result-returning `ConfigKv` replacement. This test scans the file and
//! fails the build if a contributor reintroduces a bare `.unwrap()`
//! outside any inline tests module.
//!
//! Mirrors `tests/compat/lfs_client_production_unwrap_guard.rs` but
//! intentionally lives in its own test binary so the two audits remain
//! independently grep-able. Cargo's `[[test]]` test binaries are
//! separate compilation units, so the scanning code is duplicated by
//! design rather than shared via a `pub` helper.

use std::{fs, path::PathBuf};

/// The single file this guard protects. If the file moves, update this
/// constant alongside the move so the guard keeps surfacing regressions
/// at compile-time.
const TARGET_FILE: &str = "crates/libra-core/src/internal/config.rs";

#[test]
fn config_production_has_no_bare_unwrap_calls() {
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
                 use `.expect(\"legacy Config::...\")` or migrate to `ConfigKv` instead \
                 (see CLAUDE.md error handling rules)"
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
