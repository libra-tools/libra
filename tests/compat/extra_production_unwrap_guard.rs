//! Defensive guard for the v0.17.240-252 production unwrap audit batch
//! across small utility files that don't warrant their own dedicated guard
//! binary.
//!
//! Each entry in `TARGET_FILES` was migrated to use `.expect("...")` with
//! an INVARIANT comment in the patch listed alongside it. This single
//! guard scans all of them; any production `.unwrap()` outside an inline
//! tests module trips the assertion.
//!
//! Why a combined guard for these:
//!   - The files are small and historically each had only 1-3 production
//!     `.unwrap()` calls before the audit, so an individual guard per
//!     file would be wasteful.
//!   - Cargo `[[test]]` binaries are separate compilation units, but the
//!     scan body is trivial enough to inline once and iterate over the
//!     small set.
//!
//! The lfs_client / config / head / util / client_storage guards stay
//! in dedicated binaries because those files anchor large subsystems and
//! benefit from grep-targetable test names.

use std::{fs, path::PathBuf};

/// (production file path, introducing patch version)
const TARGET_FILES: &[(&str, &str)] = &[
    ("crates/libra-core/src/utils/lfs.rs", "v0.17.240"),
    ("crates/libra-core/src/utils/object.rs", "v0.17.240"),
    ("crates/libra-core/src/utils/storage/local.rs", "v0.17.244"),
    ("crates/libra-core/src/utils/storage/tiered.rs", "v0.17.245"),
    ("crates/libra-core/src/utils/path_ext.rs", "v0.17.252"),
    ("src/git_protocol.rs", "v0.17.242"),
    ("src/lfs_structs.rs", "v0.17.242"),
    ("src/command/reflog.rs", "v0.17.251"),
    ("crates/libra-core/src/internal/reflog.rs", "v0.17.1391"),
];

#[test]
fn extra_audit_files_have_no_bare_unwrap_in_production() {
    let mut offenders = Vec::new();

    for &(target_relative_path, patch_version) in TARGET_FILES {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(target_relative_path);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read '{}': {err}", path.display()));

        let mut in_tests_module = false;

        for (idx, raw_line) in text.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = raw_line.trim_start();

            if trimmed.starts_with("#[cfg(test)]") {
                in_tests_module = true;
            }
            if in_tests_module {
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains(".unwrap()") {
                offenders.push(format!(
                    "{target_relative_path}:{line_number} reintroduces bare `.unwrap()` in \
                     production code (audited in {patch_version}): use `.expect(\"INVARIANT: ...\")` \
                     instead (see CLAUDE.md error handling rules)"
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production code in audited extra files must use INVARIANT-documented \
         `.expect(\"...\")` instead of bare `.unwrap()`:\n{}",
        offenders.join("\n"),
    );
}
