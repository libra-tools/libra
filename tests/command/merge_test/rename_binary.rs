//! MG-06: binary rename merges use Git's trivial OID rules before binary fallback.

use super::{
    assert_cli_success, create_committed_repo_via_cli, index_stage_lines, merge_expecting_conflict,
    run_libra_command, stage_blob,
};

fn binary_bytes(edit: &str) -> Vec<u8> {
    let mut bytes = b"binary\0header\n".to_vec();
    for line in 0..128 {
        bytes.extend_from_slice(format!("unchanged record {line:03}\n").as_bytes());
    }
    bytes.extend_from_slice(edit.as_bytes());
    bytes.push(b'\n');
    bytes
}

fn commit_binary_changes(repo: &std::path::Path, message: &str) {
    assert_cli_success(
        &run_libra_command(&["add", "-A", "."], repo),
        "stage binary",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], repo),
        "commit binary",
    );
}

fn assert_stage_bytes(repo: &std::path::Path, path: &str, stage: u8, expected: &[u8]) {
    let stages = index_stage_lines(repo, path);
    let stage_marker = format!(" {stage}\t");
    let line = stages
        .iter()
        .find(|line| line.contains(&stage_marker))
        .unwrap_or_else(|| panic!("missing stage {stage} at {path}: {stages:?}"));
    let blob = run_libra_command(&["cat-file", "-p", &stage_blob(line)], repo);
    assert_cli_success(&blob, "read the staged binary blob");
    assert!(
        blob.stdout == expected,
        "stage {stage} at {path} must carry Git's selected binary content; \
         actual blob {}, length {}, tail {:?}; expected length {}, tail {:?}",
        stage_blob(line),
        blob.stdout.len(),
        &blob.stdout[blob.stdout.len().saturating_sub(32)..],
        expected.len(),
        &expected[expected.len().saturating_sub(32)..],
    );
}

/// Git `merge-ort.c:2243-2246` resolves unchanged OIDs before considering file
/// type. A pure binary rename must therefore carry the other side's source edit
/// into its collision stage, without a spurious `rename involved in collision`.
#[test]
fn merge_rename_conflict_binary_collision_carries_a_one_sided_source_edit() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for ours_renames in [true, false] {
            // Given: one side only renames; the other edits the binary source
            // and independently adds the destination.
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            let source_edit = binary_bytes("source edit");
            let occupant = binary_bytes("independent destination");
            std::fs::write(p.join("old"), binary_bytes("base")).expect("base binary");
            commit_binary_changes(p, "binary base");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            let change_side = |rename: bool| {
                if rename {
                    std::fs::rename(p.join("old"), p.join("new")).expect("rename binary");
                } else {
                    std::fs::write(p.join("old"), &source_edit).expect("edit source");
                    std::fs::write(p.join("new"), &occupant).expect("add destination");
                }
            };
            change_side(ours_renames);
            commit_binary_changes(p, "ours");
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            change_side(!ours_renames);
            commit_binary_changes(p, "theirs");
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

            // When: the rename merges its source before the add/add collision.
            let output = merge_expecting_conflict(p, &["merge", "feature"], env);

            // Then: the rename stage includes the source edit, while the other
            // stage keeps the independent destination; there is no base stage.
            let (ours, theirs) = if ours_renames {
                (&source_edit, &occupant)
            } else {
                (&occupant, &source_edit)
            };
            assert_stage_bytes(p, "new", 2, ours);
            assert_stage_bytes(p, "new", 3, theirs);
            assert_eq!(index_stage_lines(p, "new").len(), 2);
            assert!(index_stage_lines(p, "old").is_empty());
            assert!(
                !String::from_utf8_lossy(&output.stdout)
                    .contains("CONFLICT (rename involved in collision)"),
                "the binary source merge is trivial and clean: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

/// Git `merge-ort.c:3021-3053` copies a clean trivial result to both renamed
/// destinations. Only a genuinely unmergeable binary preserves each original
/// separately (`t/t6422-merge-rename-corner-cases.sh:1423-1438`).
#[test]
fn merge_rename_conflict_binary_1to2_distinguishes_trivial_merge_from_fallback() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for (ours_edit, theirs_edit, expected_a, expected_b) in [
            ("base", "theirs", "theirs", "theirs"),
            ("ours", "base", "ours", "ours"),
            ("same", "same", "same", "same"),
            ("ours", "theirs", "ours", "theirs"),
        ] {
            // Given: two destinations, with unchanged, matching or divergent
            // binary contents relative to the source's base.
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            std::fs::write(p.join("old"), binary_bytes("base")).expect("base binary");
            commit_binary_changes(p, "binary base");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            std::fs::rename(p.join("old"), p.join("a")).expect("ours renames");
            std::fs::write(p.join("a"), binary_bytes(ours_edit)).expect("ours binary");
            commit_binary_changes(p, "ours");
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            std::fs::rename(p.join("old"), p.join("b")).expect("theirs renames");
            std::fs::write(p.join("b"), binary_bytes(theirs_edit)).expect("theirs binary");
            commit_binary_changes(p, "theirs");
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

            // When: content is resolved before the path-level 1to2 conflict.
            let output = merge_expecting_conflict(p, &["merge", "feature"], env);

            // Then: both index and working tree preserve Git's selected bytes.
            assert!(String::from_utf8_lossy(&output.stdout).contains("CONFLICT (rename/rename)"));
            for (path, stage, expected) in [("a", 2, expected_a), ("b", 3, expected_b)] {
                let expected = binary_bytes(expected);
                assert_stage_bytes(p, path, stage, &expected);
                assert_eq!(index_stage_lines(p, path).len(), 1);
                assert!(
                    std::fs::read(p.join(path)).expect("merged binary") == expected,
                    "{path} must contain the full binary content selected for {ours_edit}/{theirs_edit}"
                );
            }
            assert!(index_stage_lines(p, "old").is_empty());
            assert!(!p.join("old").exists());
        }
    }
}
