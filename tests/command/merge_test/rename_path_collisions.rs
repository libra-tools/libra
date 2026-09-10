//! MG-06: path conflicts survive content preferences and preserve colliding adds.

use std::{collections::BTreeMap, path::Path};

use super::{
    assert_cli_success, create_committed_repo_via_cli, head_commit, index_stage_lines,
    merge_expecting_conflict, parse_json_stdout, run_libra_command,
    run_libra_command_with_stdin_and_env, stage_blob,
};

#[derive(Clone, Copy)]
enum Shape {
    Deleted,
    Divergent,
}

fn collision_repo(shape: Shape, binary: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let content = |edit: &str| {
        let mut bytes = format!("l1\n{edit}\nl3\nl4\nl5\nl6\nl7\nl8\n").into_bytes();
        if binary {
            bytes.push(0);
        }
        bytes
    };
    let commit_all = |message: &str| {
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage fixture");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
            "commit fixture",
        );
    };
    std::fs::write(p.join("old"), content("l2")).expect("base");
    commit_all("base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("old"), p.join("a")).expect("ours renames");
    if matches!(shape, Shape::Divergent) && !binary {
        std::fs::write(p.join("a"), content("OURS")).expect("ours edits");
    }
    commit_all("ours renames");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    match shape {
        Shape::Deleted => std::fs::remove_file(p.join("old")).expect("theirs deletes"),
        Shape::Divergent => {
            std::fs::rename(p.join("old"), p.join("b")).expect("theirs renames");
            if !binary {
                std::fs::write(p.join("b"), content("THEIRS")).expect("theirs edits");
            }
        }
    }
    let added: &[u8] = if binary {
        b"INDEPENDENT ADD\0\n"
    } else {
        b"INDEPENDENT ADD\n"
    };
    std::fs::write(p.join("a"), added).expect("theirs independently adds");
    commit_all("theirs collides");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

fn blob_at_stage(repo: &Path, path: &str, stage: u8) -> Vec<u8> {
    let entries = index_stage_lines(repo, path);
    let entry = entries
        .iter()
        .find(|line| line.contains(&format!(" {stage}\t")))
        .expect("expected unmerged stage");
    let blob = run_libra_command(&["cat-file", "-p", &stage_blob(entry)], repo);
    assert_cli_success(&blob, "read conflict stage");
    blob.stdout
}

fn check_collision(shape: Shape, binary: bool) {
    let mut summaries = BTreeMap::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for reverse in [false, true] {
            for variant in ["normal", "ours", "theirs", "diff3"] {
                // Given: the renaming destination is also an independent add.
                let repo = collision_repo(shape, binary);
                let p = repo.path();
                let target = if reverse {
                    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "reverse");
                    "main"
                } else {
                    "feature"
                };
                if variant == "diff3" {
                    assert_cli_success(
                        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
                        "diff3",
                    );
                }
                let original_head = head_commit(p);
                let original_a = std::fs::read(p.join("a")).expect("ours before merge");
                let mut args = vec!["merge", target];
                if matches!(variant, "ours" | "theirs") {
                    args.extend(["-X", variant]);
                }
                let mut preview_args = args.clone();
                preview_args.extend(["--dry-run", "--json"]);
                let preview = run_libra_command_with_stdin_and_env(&preview_args, p, "", env);
                assert_eq!(
                    preview.status.code(),
                    Some(1),
                    "preview path conflict: {preview:?}"
                );
                let mut preview = parse_json_stdout(&preview);
                let expected = match shape {
                    Shape::Deleted => serde_json::json!([{"path":"a", "kind":"content"}]),
                    Shape::Divergent => serde_json::json!([
                        {"path":"a", "kind":"content"},
                        {"path":"b", "kind":"rename-rename"},
                    ]),
                };
                assert_eq!(preview["data"]["conflict_kinds"], expected);
                let data = preview["data"].as_object_mut().expect("preview data");
                data.remove("old_commit");
                data.remove("commit");
                if let Some(previous) = summaries.insert((reverse, variant), preview.clone()) {
                    assert_eq!(
                        preview, previous,
                        "walk summaries including files_changed must agree"
                    );
                }

                // When: the CLI applies the same merge, including content favor.
                let output = merge_expecting_conflict(p, &args, env);

                // Then: the path stays unmerged, both original contributions
                // remain addressable, and the worktree shows the selected content.
                assert_eq!(head_commit(p), original_head);
                assert!(String::from_utf8_lossy(&output.stdout).contains("CONFLICT (rename/"));
                let stages = index_stage_lines(p, "a");
                assert_eq!(stages.len(), 2, "{variant}: {stages:?}");
                let ours = blob_at_stage(p, "a", 2);
                let theirs = blob_at_stage(p, "a", 3);
                let actual = std::fs::read(p.join("a")).expect("destination worktree");
                let added = if reverse { &ours } else { &theirs };
                assert!(added.starts_with(b"INDEPENDENT ADD"));
                match variant {
                    "ours" => assert_eq!(actual, ours),
                    "theirs" => assert_eq!(actual, theirs),
                    "normal" | "diff3" if binary => assert_eq!(actual, original_a),
                    "normal" | "diff3" => {
                        let text = String::from_utf8(actual).expect("text conflict");
                        let outer_marker = text
                            .lines()
                            .find_map(|line| {
                                line.strip_suffix(" HEAD").filter(|marker| {
                                    marker.len() >= 7 && marker.bytes().all(|byte| byte == b'<')
                                })
                            })
                            .expect("outer HEAD marker");
                        assert!(text.contains("INDEPENDENT ADD\n"), "{text}");
                        if variant == "diff3" {
                            let width = outer_marker.len();
                            assert!(
                                text.contains(&format!(
                                    "{} base\n{}\n",
                                    "|".repeat(width),
                                    "=".repeat(width)
                                )),
                                "{text}"
                            );
                        }
                        if matches!(shape, Shape::Divergent) {
                            assert!(text.contains("<<<<<<<< "), "{text}");
                            assert!(
                                text.contains("OURS\n") && text.contains("THEIRS\n"),
                                "{text}"
                            );
                        }
                    }
                    other => panic!("unknown fixture variant {other}"),
                }
                if matches!(shape, Shape::Divergent) {
                    let rename_stage = if reverse { 3 } else { 2 };
                    let opposite_stage = if reverse { 2 } else { 3 };
                    assert_eq!(
                        blob_at_stage(p, "a", rename_stage),
                        blob_at_stage(p, "b", opposite_stage)
                    );
                    assert_eq!(index_stage_lines(p, "b").len(), 1);
                }
                assert!(index_stage_lines(p, "old").is_empty());
                assert!(!p.join("old").exists());
            }
        }
    }
}

#[test]
fn rename_add_delete_keeps_the_path_conflict_under_strategy_preferences() {
    check_collision(Shape::Deleted, false);
}

#[test]
fn rename_1to2_collision_shows_the_independent_add_and_preserves_both_stages() {
    check_collision(Shape::Divergent, false);
}

#[test]
fn binary_rename_path_collisions_keep_bytes_and_unmerged_stages() {
    check_collision(Shape::Deleted, true);
    check_collision(Shape::Divergent, true);
}

#[test]
fn an_empty_colliding_add_cannot_erase_rename_content_under_strategy_preferences() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for shape in [Shape::Deleted, Shape::Divergent] {
            for reverse in [false, true] {
                for favor in ["ours", "theirs"] {
                    // Given: the independent add contributes no text hunks.
                    let repo = collision_repo(shape, false);
                    let p = repo.path();
                    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
                    super::commit_file(p, "a", "", "empty independent add");
                    let target = if reverse {
                        "main"
                    } else {
                        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
                        "feature"
                    };

                    // When: either preference resolves only actual content hunks.
                    merge_expecting_conflict(p, &["merge", target, "-X", favor], env);

                    // Then: the empty side cannot remove the nonempty rename,
                    // and both original contributions remain in unmerged stages.
                    let renamed = blob_at_stage(p, "a", if reverse { 3 } else { 2 });
                    assert!(!renamed.is_empty());
                    assert_eq!(std::fs::read(p.join("a")).expect("worktree"), renamed);
                    assert!(blob_at_stage(p, "a", if reverse { 2 } else { 3 }).is_empty());
                    assert_eq!(index_stage_lines(p, "a").len(), 2);
                }
            }
        }
    }
}
