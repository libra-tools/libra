//! Fixtures and expected conflict reports shared by the rename behavior matrix.

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

/// Both source merges must survive the final working-tree conflict, so
/// resolving it with add cannot discard the first source's other-side edit.
#[test]
fn merge_rename_conflict_2to1_worktree_keeps_both_source_edits() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = rename_2to1_repo();
        super::merge_expecting_conflict(repo.path(), &["merge", "feature"], env);
        let content = std::fs::read_to_string(repo.path().join("new")).expect("collision");
        assert!(
            content.contains("THEIRS") && content.contains("OURS"),
            "both source edits survive: {content}"
        );
    }
}

pub(super) fn expected_conflict_kinds(shape: &str) -> serde_json::Value {
    match shape {
        "1to2 clean content" | "1to2 conflicting content" => serde_json::json!([
            {"path": "a", "kind": "rename-rename"},
            {"path": "b", "kind": "rename-rename"},
        ]),
        "rename/delete, ours renames" | "rename/delete, theirs renames" => {
            serde_json::json!([{"path": "new", "kind": "modify-delete"}])
        }
        "rename/add" | "2to1" | "1to1 conflicting" => {
            serde_json::json!([{"path": "new", "kind": "content"}])
        }
        other => panic!("missing conflict-report expectation for rename shape {other}"),
    }
}

/// Distinct sources move to the same destination while the other side edits
/// each source in place. The rename merges are clean; their collision is not.
pub(super) fn rename_2to1_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let body = |tag: &str, edit: Option<&str>| -> String {
        (1..=8)
            .map(|line| match (line, edit) {
                (3, Some(text)) => format!("{text}\n"),
                _ => format!("{tag}{line}\n"),
            })
            .collect()
    };
    std::fs::write(p.join("o1"), body("a", None)).expect("first source");
    std::fs::write(p.join("o2"), body("b", None)).expect("second source");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base sources", "--no-verify"], p),
        "commit base sources",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");

    std::fs::rename(p.join("o1"), p.join("new")).expect("ours moves first source");
    std::fs::write(p.join("o2"), body("b", Some("OURS"))).expect("ours edits second source");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage ours");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours moves o1", "--no-verify"], p),
        "commit ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    std::fs::rename(p.join("o2"), p.join("new")).expect("theirs moves second source");
    std::fs::write(p.join("o1"), body("a", Some("THEIRS"))).expect("theirs edits first source");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage theirs");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs moves o2", "--no-verify"], p),
        "commit theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}
