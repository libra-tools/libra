//! MG-06: production merges consume every path-conflict shape in a virtual base.

use std::{collections::BTreeSet, path::Path};

use super::{
    assert_cli_success, commit_file, create_committed_repo_via_cli, index_stage_lines,
    merge_expecting_conflict, rename_1to2_repo, rename_add_repo, rename_delete_repo,
    run_libra_command, stage_blob, unmerged_stage_lines,
};

#[derive(Clone, Copy, Debug)]
enum Shape {
    OneToTwo,
    TwoToOne,
    RenameDelete,
    RenameAdd,
}

impl Shape {
    const fn destinations(self) -> &'static [&'static str] {
        match self {
            Self::OneToTwo => &["a", "b"],
            Self::TwoToOne | Self::RenameDelete | Self::RenameAdd => &["new"],
        }
    }

    const fn sources(self) -> &'static [&'static str] {
        match self {
            Self::TwoToOne => &["old_a", "old_b"],
            Self::OneToTwo | Self::RenameDelete | Self::RenameAdd => &["old"],
        }
    }
}

fn numbered(prefix: &str, replacements: &[(usize, &str)]) -> String {
    (1..=8)
        .map(|line| match replacements.iter().find(|(n, _)| *n == line) {
            Some((_, text)) => format!("{text}\n"),
            None => format!("{prefix}{line}\n"),
        })
        .collect()
}

fn commit_worktree(p: &Path) {
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage changes");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "side changes", "--no-verify"], p),
        "commit changes",
    );
}

fn bases(shape: Shape) -> tempfile::TempDir {
    match shape {
        Shape::OneToTwo => rename_1to2_repo(2, 6),
        Shape::RenameDelete => {
            let repo = rename_delete_repo(true);
            commit_file(
                repo.path(),
                "new",
                &numbered("l", &[(2, "OURS")]),
                "edit rename",
            );
            repo
        }
        Shape::RenameAdd => {
            let repo = rename_add_repo();
            let p = repo.path();
            commit_file(p, "new", &numbered("l", &[(2, "OURS")]), "edit rename");
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            commit_file(p, "old", &numbered("l", &[(6, "THEIRS")]), "edit original");
            repo
        }
        Shape::TwoToOne => {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            commit_file(p, "old_a", &numbered("a", &[]), "first source");
            commit_file(p, "old_b", &numbered("b", &[]), "second source");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            std::fs::rename(p.join("old_a"), p.join("new")).expect("rename first source");
            std::fs::write(p.join("old_b"), numbered("b", &[(6, "B_EDIT")])).expect("edit b");
            commit_worktree(p);
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            std::fs::rename(p.join("old_b"), p.join("new")).expect("rename second source");
            std::fs::write(p.join("old_a"), numbered("a", &[(2, "A_EDIT")])).expect("edit a");
            commit_worktree(p);
            repo
        }
    }
}

fn crisscross(shape: Shape) -> tempfile::TempDir {
    let repo = bases(shape);
    let p = repo.path();
    for (from, other, tip) in [("main", "feature", "x"), ("feature", "main", "y")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout base");
        assert_cli_success(
            &run_libra_command(&["checkout", "-b", tip], p),
            "create arm",
        );
        merge_expecting_conflict(p, &["merge", other], &[]);
        for path in shape.destinations() {
            std::fs::write(p.join(path), format!("{tip} resolution\n")).expect("resolve arm");
        }
        assert_cli_success(
            &run_libra_command(&["add", "-A", "."], p),
            "stage resolution",
        );
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "commit two-parent arm",
        );
    }
    let output = run_libra_command(&["merge-base", "--all", "x", "y"], p);
    assert_cli_success(&output, "enumerate criss-cross bases");
    let actual: BTreeSet<_> = String::from_utf8(output.stdout)
        .expect("base ids")
        .lines()
        .map(str::to_owned)
        .collect();
    let expected: BTreeSet<_> = ["main", "feature"]
        .map(|branch| {
            let output = run_libra_command(&["rev-parse", branch], p);
            assert_cli_success(&output, "read base branch");
            String::from_utf8(output.stdout)
                .expect("branch id")
                .trim()
                .to_owned()
        })
        .into_iter()
        .collect();
    assert_eq!(
        actual.len(),
        2,
        "the production merge must actually fold two bases"
    );
    assert_eq!(
        actual, expected,
        "both rename-conflicting commits are merge bases"
    );
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout ours");
    repo
}

fn assert_ancestor(shape: Shape, ancestor: &str) {
    match shape {
        Shape::OneToTwo => assert_eq!(ancestor, numbered("l", &[(2, "OURS"), (6, "THEIRS")])),
        Shape::RenameDelete => assert_eq!(ancestor, numbered("l", &[])),
        Shape::TwoToOne | Shape::RenameAdd => {
            // Git merge-ort.c:3135-3169 merges each source first; :4335-4349
            // then stores the destination's add/add conflict inside the base.
            assert!(
                ancestor.contains("<<<<<<<<<") && ancestor.contains(">>>>>>>>>"),
                "{ancestor}"
            );
            let fragments = match shape {
                Shape::TwoToOne => vec![
                    numbered("a", &[(2, "A_EDIT")]),
                    numbered("b", &[(6, "B_EDIT")]),
                ],
                Shape::RenameAdd => vec![
                    numbered("l", &[(2, "OURS"), (6, "THEIRS")]),
                    "theirs own file\n".to_owned(),
                ],
                Shape::OneToTwo | Shape::RenameDelete => unreachable!("handled above"),
            };
            for fragment in fragments {
                assert!(
                    ancestor.contains(&fragment),
                    "{shape:?}: source merge lost inside collision: {ancestor}"
                );
            }
        }
    }
}

fn assert_fold_consumed(shape: Shape) {
    // Given: two independently resolved arms with the exact two bases above.
    let repo = crisscross(shape);
    let p = repo.path();
    let target = run_libra_command(&["rev-parse", "y"], p);
    assert_cli_success(&target, "read the target commit label");
    let target_id = String::from_utf8(target.stdout).expect("target id");
    let target_label = target_id.get(..7).expect("seven hex digits");
    let width = match shape {
        Shape::OneToTwo | Shape::RenameDelete => 7,
        Shape::TwoToOne | Shape::RenameAdd => 10,
    };
    let expected_body = format!(
        "{} HEAD\nx resolution\n{}\ny resolution\n{} {target_label}\n",
        "<".repeat(width),
        "=".repeat(width),
        ">".repeat(width)
    );
    let mut walks = Vec::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // When: a real merge consumes the virtual ancestor and writes its index.
        merge_expecting_conflict(p, &["merge", "y"], env);
        // Then: stage 1 exposes the ancestor used by the outer content merge.
        let mut contents = Vec::new();
        for path in shape.destinations() {
            let stages = index_stage_lines(p, path);
            assert_eq!(
                stages.len(),
                3,
                "{shape:?}: {path} must have all three content stages"
            );
            let base = stages
                .iter()
                .find(|line| line.contains(" 1\t"))
                .expect("virtual base stage");
            let output = run_libra_command(&["cat-file", "-p", &stage_blob(base)], p);
            assert_cli_success(&output, "read virtual ancestor blob");
            let ancestor = String::from_utf8(output.stdout).expect("ancestor text");
            assert_ancestor(shape, &ancestor);
            let body = std::fs::read_to_string(p.join(path)).expect("outer conflict");
            assert_eq!(
                body, expected_body,
                "{shape:?}: complete outer conflict and commit labels"
            );
            contents.push((ancestor, body));
        }
        for path in shape.sources() {
            assert!(
                index_stage_lines(p, path).is_empty(),
                "{shape:?}: source {path} resurrected"
            );
            assert!(
                !p.join(path).exists(),
                "{shape:?}: source {path} returned to worktree"
            );
        }
        let stages = unmerged_stage_lines(p);
        assert_eq!(
            stages.len(),
            3 * shape.destinations().len(),
            "unexpected extra conflict"
        );
        walks.push((stages, contents));
        assert_cli_success(
            &run_libra_command(&["merge", "--abort"], p),
            "restore same two tips",
        );
    }
    assert_eq!(
        walks[0], walks[1],
        "{shape:?}: both walks consume identical ancestor blobs and paths"
    );
}

#[test]
fn merge_fold_1to2_uses_merged_content_at_both_destinations() {
    assert_fold_consumed(Shape::OneToTwo);
}

#[test]
fn merge_fold_2to1_preserves_both_source_merges_inside_collision() {
    assert_fold_consumed(Shape::TwoToOne);
}

#[test]
fn merge_fold_rename_delete_uses_original_base_at_destination() {
    assert_fold_consumed(Shape::RenameDelete);
}

#[test]
fn merge_fold_rename_add_preserves_source_merge_and_destination_occupant() {
    assert_fold_consumed(Shape::RenameAdd);
}
