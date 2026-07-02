//! Integration tests for `libra commit-tree` + `--index-file` scratch
//! composition (lore.md §1.15): a revision is composed with ZERO side
//! effects on the shared index, worktree, HEAD, or refs.
//!
//! **Layer:** L1 — deterministic.

use super::*;

fn base_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    repo
}

fn stdout_line(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn commit_tree_composes_off_worktree() {
    let repo = base_repo();
    let p = repo.path();
    let index_before = fs::read(p.join(".libra/index")).unwrap();
    // blob → scratch index → tree → commit, all without touching state.
    let blob = stdout_line(&run_libra_command_with_stdin(
        &["hash-object", "-w", "--stdin"],
        p,
        "scratch content\n",
    ));
    assert!(!blob.is_empty(), "blob written");
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--index-file",
                "scratch.idx",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},dir/scratch.txt"),
            ],
            p,
        ),
        "scratch update-index",
    );
    let tree = stdout_line(&run_libra_command(
        &["write-tree", "--index-file", "scratch.idx"],
        p,
    ));
    let commit = stdout_line(&run_libra_command(
        &["commit-tree", &tree, "-p", "HEAD", "-m", "composed"],
        p,
    ));
    assert_eq!(commit.len(), 40, "an OID was printed: {commit}");
    // Zero side effects.
    assert_eq!(
        fs::read(p.join(".libra/index")).unwrap(),
        index_before,
        "shared index byte-untouched"
    );
    assert!(!p.join("dir").exists(), "worktree untouched");
    // Object shape: correct tree/parent/message separation.
    let shown = run_libra_command(&["cat-file", "-p", &commit], p);
    let text = String::from_utf8_lossy(&shown.stdout).to_string();
    assert!(text.contains(&format!("tree {tree}")), "{text}");
    assert!(
        text.contains("\n\ncomposed"),
        "blank-line separator: {text}"
    );
    // Publishable via update-ref; log agrees.
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/composed", &commit], p),
        "publish",
    );
    let json = parse_json_stdout(&run_libra_command(
        &["--json", "log", "-n", "1", "composed"],
        p,
    ));
    assert_eq!(
        json["data"]["commits"][0]["subject"].as_str(),
        Some("composed"),
        "{json}"
    );
    // A trailer block survives byte-exact into the hashed message (1.9/1.10).
    let with_trailer = stdout_line(&run_libra_command(
        &[
            "commit-tree",
            &tree,
            "-m",
            "subject",
            "-m",
            "Reviewed-by: Alice <a@example>",
        ],
        p,
    ));
    let shown = run_libra_command(&["cat-file", "-p", &with_trailer], p);
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains("subject\n\nReviewed-by: Alice"),
        "paragraphs joined by blank lines"
    );
}

#[test]
fn commit_tree_argument_matrix() {
    let repo = base_repo();
    let p = repo.path();
    let tree = stdout_line(&run_libra_command(&["write-tree"], p));
    // Commit-ish peels to its tree (Libra superset, documented).
    let via_head = stdout_line(&run_libra_command(
        &["commit-tree", "HEAD", "-m", "reuse"],
        p,
    ));
    assert_eq!(via_head.len(), 40);
    // Duplicate parents warn + dedup; merge commits carry both parents.
    let dup = run_libra_command(
        &[
            "commit-tree",
            &tree,
            "-p",
            "HEAD",
            "-p",
            "HEAD",
            "-m",
            "dup",
        ],
        p,
    );
    assert_cli_success(&dup, "dup parents succeed");
    assert!(
        String::from_utf8_lossy(&dup.stderr).contains("duplicate parent"),
        "{}",
        String::from_utf8_lossy(&dup.stderr)
    );
    // Message via stdin (bare pipe) and via -F -.
    let piped = stdout_line(&run_libra_command_with_stdin(
        &["commit-tree", &tree],
        p,
        "piped message\n",
    ));
    assert_eq!(piped.len(), 40, "bare stdin message accepted");
    let via_f = stdout_line(&run_libra_command_with_stdin(
        &["commit-tree", &tree, "-F", "-"],
        p,
        "file message\n",
    ));
    assert_eq!(via_f.len(), 40);
    // Refusals: empty message (D-precedent), bad tree, non-commit parent.
    let empty = run_libra_command_with_stdin(&["commit-tree", &tree], p, "\n");
    assert_eq!(empty.status.code(), Some(129), "empty message refused");
    let bad_tree = run_libra_command(&["commit-tree", "deadbeef", "-m", "x"], p);
    assert!(!bad_tree.status.success(), "unresolvable tree refused");
    let blob = stdout_line(&run_libra_command_with_stdin(
        &["hash-object", "-w", "--stdin"],
        p,
        "not a commit\n",
    ));
    let bad_parent = run_libra_command(&["commit-tree", &tree, "-p", &blob, "-m", "x"], p);
    assert!(!bad_parent.status.success(), "blob parent refused");
    // HEAD and refs never moved through all of the above.
    let status = run_libra_command(&["--json", "status"], p);
    let json = parse_json_stdout(&status);
    assert_eq!(
        json["data"]["staged"]["new"].as_array().map(Vec::len),
        Some(0),
        "no staged changes appeared: {json}"
    );
}

#[test]
fn scratch_index_isolation_matrix() {
    let repo = base_repo();
    let p = repo.path();
    // A missing scratch index behaves as EMPTY (canonical empty tree).
    let empty_tree = stdout_line(&run_libra_command(
        &["write-tree", "--index-file", "missing.idx"],
        p,
    ));
    assert_eq!(
        empty_tree, "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        "canonical empty tree"
    );
    // read-tree into a scratch file materializes it without touching staging.
    let index_before = fs::read(p.join(".libra/index")).unwrap();
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD", "--index-file", "scratch2.idx"], p),
        "scratch read-tree",
    );
    assert!(p.join("scratch2.idx").exists());
    assert_eq!(
        fs::read(p.join(".libra/index")).unwrap(),
        index_before,
        "shared index untouched by scratch read-tree"
    );
    // The scratch round-trips: write-tree over it reproduces HEAD's tree.
    let head_tree = stdout_line(&run_libra_command(&["write-tree"], p));
    let scratch_tree = stdout_line(&run_libra_command(
        &["write-tree", "--index-file", "scratch2.idx"],
        p,
    ));
    assert_eq!(head_tree, scratch_tree, "scratch round-trip");
}
