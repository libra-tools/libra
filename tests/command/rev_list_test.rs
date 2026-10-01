//! Integration tests for `rev-list` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use git_internal::hash::{HashKind, set_hash_kind_for_test};

use super::*;

fn create_two_commit_repo_with_direct_tip_update(timestamp_offset: usize) -> tempfile::TempDir {
    let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
    let repo = create_committed_repo_via_cli();
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    runtime.block_on(async {
        let _guard = ChangeDirGuard::new(repo.path());
        let parent_id = Head::current_commit().await.expect("expected HEAD commit");
        let parent: Commit = load_object(&parent_id).expect("failed to load parent commit");
        let mut author = parent.author.clone();
        let mut committer = parent.committer.clone();
        author.timestamp = parent.committer.timestamp + timestamp_offset;
        committer.timestamp = parent.committer.timestamp + timestamp_offset;
        let commit = Commit::new(author, committer, parent.tree_id, vec![parent_id], "second");
        save_object(&commit, &commit.id).expect("failed to save second commit");
        Branch::update_branch("main", &commit.id.to_string(), None)
            .await
            .expect("failed to update main branch");
    });

    repo
}

#[path = "rev_list_output_test.rs"]
mod rev_list_output_test;

#[path = "rev_list_parent_filter_test.rs"]
mod rev_list_parent_filter_test;

#[path = "rev_list_range_test.rs"]
mod rev_list_range_test;

#[path = "rev_list_date_filter_test.rs"]
mod rev_list_date_filter_test;

#[path = "rev_list_first_parent_test.rs"]
mod rev_list_first_parent_test;

#[path = "rev_list_author_filter_test.rs"]
mod rev_list_author_filter_test;
#[path = "rev_list_cherry_filter_test.rs"]
mod rev_list_cherry_filter_test;
#[path = "rev_list_cherry_shorthand_test.rs"]
mod rev_list_cherry_shorthand_test;
#[path = "rev_list_children_test.rs"]
mod rev_list_children_test;
#[path = "rev_list_committer_filter_test.rs"]
mod rev_list_committer_filter_test;
#[path = "rev_list_grep_filter_test.rs"]
mod rev_list_grep_filter_test;
#[path = "rev_list_path_filter_test.rs"]
mod rev_list_path_filter_test;

#[test]
fn test_rev_list_defaults_to_head() {
    let repo = create_committed_repo_via_cli();

    let implicit = run_libra_command(&["rev-list"], repo.path());
    assert_cli_success(&implicit, "rev-list");

    let explicit = run_libra_command(&["rev-list", "HEAD"], repo.path());
    assert_cli_success(&explicit, "rev-list HEAD");

    assert_eq!(implicit.stdout, explicit.stdout);
}

#[test]
fn test_rev_list_head_lists_reachable_commits_newest_first() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "rev-parse HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let parent = run_libra_command(&["rev-parse", "HEAD~1"], repo.path());
    assert_cli_success(&parent, "rev-parse HEAD~1");
    let parent_hash = String::from_utf8_lossy(&parent.stdout).trim().to_string();

    let output = run_libra_command(&["rev-list", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-list HEAD");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines, vec![head_hash.as_str(), parent_hash.as_str()]);
}

#[test]
fn test_rev_list_preserves_traversal_order_for_equal_timestamps() {
    let repo = create_two_commit_repo_with_direct_tip_update(0);

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "rev-parse HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let parent = run_libra_command(&["rev-parse", "HEAD~1"], repo.path());
    assert_cli_success(&parent, "rev-parse HEAD~1");
    let parent_hash = String::from_utf8_lossy(&parent.stdout).trim().to_string();

    let output = run_libra_command(&["rev-list", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-list HEAD with equal timestamps");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines, vec![head_hash.as_str(), parent_hash.as_str()]);
}

#[test]
fn test_rev_list_supports_revision_navigation() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);

    let parent = run_libra_command(&["rev-parse", "HEAD~1"], repo.path());
    assert_cli_success(&parent, "rev-parse HEAD~1");
    let parent_hash = String::from_utf8_lossy(&parent.stdout).trim().to_string();

    let output = run_libra_command(&["rev-list", "HEAD~1"], repo.path());
    assert_cli_success(&output, "rev-list HEAD~1");

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), parent_hash);
}

#[test]
fn test_rev_list_max_count_and_skip_limit_visible_output() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);

    let full = run_libra_command(&["rev-list", "HEAD"], repo.path());
    assert_cli_success(&full, "rev-list HEAD");
    let full_stdout = String::from_utf8_lossy(&full.stdout);
    let full_lines = full_stdout.lines().collect::<Vec<_>>();
    assert_eq!(full_lines.len(), 2, "expected two commits: {full_stdout}");

    let limited = run_libra_command(&["rev-list", "--max-count", "1", "HEAD"], repo.path());
    assert_cli_success(&limited, "rev-list --max-count 1 HEAD");
    let limited_stdout = String::from_utf8_lossy(&limited.stdout);
    assert_eq!(
        limited_stdout.lines().collect::<Vec<_>>(),
        vec![full_lines[0]]
    );

    let short_limited = run_libra_command(&["rev-list", "-n", "1", "HEAD"], repo.path());
    assert_cli_success(&short_limited, "rev-list -n 1 HEAD");
    assert_eq!(short_limited.stdout, limited.stdout);

    let skipped = run_libra_command(
        &["rev-list", "--skip", "1", "--max-count", "1", "HEAD"],
        repo.path(),
    );
    assert_cli_success(&skipped, "rev-list --skip 1 --max-count 1 HEAD");
    let skipped_stdout = String::from_utf8_lossy(&skipped.stdout);
    assert_eq!(
        skipped_stdout.lines().collect::<Vec<_>>(),
        vec![full_lines[1]]
    );
}

#[test]
fn test_rev_list_count_reports_filtered_commit_count() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);

    let all = run_libra_command(&["rev-list", "--count", "HEAD"], repo.path());
    assert_cli_success(&all, "rev-list --count HEAD");
    assert_eq!(String::from_utf8_lossy(&all.stdout).trim(), "2");

    let limited = run_libra_command(
        &[
            "rev-list",
            "--count",
            "--skip",
            "1",
            "--max-count",
            "1",
            "HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&limited, "rev-list --count --skip 1 --max-count 1 HEAD");
    assert_eq!(String::from_utf8_lossy(&limited.stdout).trim(), "1");
}

#[test]
fn test_rev_list_invalid_target_returns_cli_error_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["rev-list", "badref"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.contains("not a valid object name: 'badref'"));
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn test_rev_list_rejects_tag_object_that_points_to_tree() {
    let repo = create_committed_repo_via_cli();
    let tag_id = create_non_commit_tag_object(repo.path());

    let output = run_libra_command(&["rev-list", tag_id.as_str()], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.contains("not a valid object name"));
    assert!(
        stderr.contains("tag points to tree"),
        "expected typed tag-target detail, got: {stderr}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_list_accepts_fully_qualified_remote_tracking_ref() {
    let repo = tempfile::tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .expect("failed to create remote-tracking ref");

    let output = run_libra_command(&["rev-list", "refs/remotes/origin/main"], repo.path());
    assert_cli_success(&output, "rev-list refs/remotes/origin/main");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        head.to_string()
    );
}

#[test]
fn test_rev_list_reverse_outputs_oldest_first() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);
    let p = repo.path();
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let parent = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
        .trim()
        .to_string();

    // --reverse flips the default newest-first order to oldest-first.
    let out = run_libra_command(&["rev-list", "--reverse", "HEAD"], p);
    assert_cli_success(&out, "rev-list --reverse HEAD");
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![parent.clone(), head.clone()],
        "reverse = oldest first"
    );

    // Commit limiting is applied BEFORE the reverse: `-n 1` selects the newest
    // (head); reversing a single commit is still head. (Were it reverse-first,
    // the output would be the parent.)
    let out2 = run_libra_command(&["rev-list", "-n", "1", "--reverse", "HEAD"], p);
    assert_cli_success(&out2, "rev-list -n 1 --reverse HEAD");
    let lines2 = String::from_utf8_lossy(&out2.stdout)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        lines2,
        vec![head.clone()],
        "limit-then-reverse keeps the newest"
    );
}

#[test]
fn test_rev_list_all_includes_every_ref() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let c1 = rev("HEAD");

    // A divergent commit on branch `other`.
    assert_cli_success(&run_libra_command(&["branch", "other"], p), "branch other");
    assert_cli_success(&run_libra_command(&["switch", "other"], p), "switch other");
    std::fs::write(p.join("other.txt"), "o\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "other.txt"], p), "add other");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = rev("HEAD");

    // Advance main with its own commit.
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("main.txt"), "m\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "commit c3",
    );
    let c3 = rev("HEAD");

    // `rev-list main` sees main's history but not the `other` branch tip.
    let main_only = run_libra_command(&["rev-list", "main"], p);
    assert_cli_success(&main_only, "rev-list main");
    let s = String::from_utf8_lossy(&main_only.stdout).into_owned();
    assert!(s.contains(&c3) && s.contains(&c1), "main has c3+c1: {s:?}");
    assert!(!s.contains(&c2), "main must not see other's c2: {s:?}");

    // `rev-list --all` includes every ref tip's history.
    let all = run_libra_command(&["rev-list", "--all"], p);
    assert_cli_success(&all, "rev-list --all");
    let s_all = String::from_utf8_lossy(&all.stdout).into_owned();
    assert!(
        s_all.contains(&c1) && s_all.contains(&c2) && s_all.contains(&c3),
        "--all must include c1, c2 (other), and c3 (main): {s_all:?}"
    );
}

#[test]
fn test_rev_list_all_includes_tag_only_commits() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let c1 = rev("HEAD");

    // Commit c2 and tag it, then reset main back to c1 so c2 is reachable ONLY
    // via the tag (not from any branch).
    std::fs::write(p.join("t.txt"), "t\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "t.txt"], p), "add t");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = rev("HEAD");
    assert_cli_success(&run_libra_command(&["tag", "mytag"], p), "tag mytag at c2");
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &c1], p),
        "reset main to c1",
    );

    // main no longer reaches c2.
    let main_only = run_libra_command(&["rev-list", "main"], p);
    assert_cli_success(&main_only, "rev-list main");
    assert!(
        !String::from_utf8_lossy(&main_only.stdout).contains(&c2),
        "main must not reach the tag-only commit c2"
    );

    // --all reaches c2 through the tag (seeded by object id, not name).
    let all = run_libra_command(&["rev-list", "--all"], p);
    assert_cli_success(&all, "rev-list --all");
    let s = String::from_utf8_lossy(&all.stdout).into_owned();
    assert!(
        s.contains(&c1) && s.contains(&c2),
        "--all must include the tag-only commit c2: {s:?}"
    );
}

#[test]
fn test_rev_list_all_on_unborn_repo_is_empty() {
    // `--all` supplies the ref set as input; with no refs the output is empty
    // (exit 0), not a fallback to an unborn HEAD error.
    let repo = tempfile::tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let out = run_libra_command(&["rev-list", "--all"], repo.path());
    assert_cli_success(&out, "rev-list --all on an unborn repo");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "no refs -> empty output: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_rev_list_all_includes_detached_head_commit() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let c1 = rev("HEAD");

    // Detach HEAD, then make a commit reachable only from the detached HEAD.
    assert_cli_success(
        &run_libra_command(&["switch", "--detach", &c1], p),
        "detach HEAD",
    );
    std::fs::write(p.join("d.txt"), "d\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "d.txt"], p), "add d");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "detached", "--no-verify"], p),
        "commit detached",
    );
    let cdet = rev("HEAD");
    assert_ne!(cdet, c1, "detached commit must advance HEAD");

    // main does not reach the detached commit.
    let main_only = run_libra_command(&["rev-list", "main"], p);
    assert_cli_success(&main_only, "rev-list main");
    assert!(
        !String::from_utf8_lossy(&main_only.stdout).contains(&cdet),
        "main must not reach the detached commit"
    );

    // --all includes the detached HEAD commit (Git seeds --all with HEAD too).
    let all = run_libra_command(&["rev-list", "--all"], p);
    assert_cli_success(&all, "rev-list --all");
    assert!(
        String::from_utf8_lossy(&all.stdout).contains(&cdet),
        "--all must include the detached HEAD commit: {}",
        String::from_utf8_lossy(&all.stdout)
    );
}

#[test]
fn test_rev_list_date_order_matches_default_ordering() {
    let repo = create_two_commit_repo_with_direct_tip_update(1);
    let p = repo.path();
    // --date-order is accepted and produces Libra's default committer-date
    // (newest-first) ordering.
    let default = run_libra_command(&["rev-list", "HEAD"], p);
    assert_cli_success(&default, "rev-list HEAD");
    let dated = run_libra_command(&["rev-list", "--date-order", "HEAD"], p);
    assert_cli_success(&dated, "rev-list --date-order HEAD");
    assert_eq!(
        String::from_utf8_lossy(&default.stdout),
        String::from_utf8_lossy(&dated.stdout),
        "--date-order matches the default ordering"
    );
}

#[test]
fn test_rev_list_boundary() {
    // `--boundary <range>` lists the included commits, then the excluded frontier
    // commits (parents of an included commit that fall in the excluded set) each
    // prefixed with `-`, after the listed commits. Matches git rev-list --boundary.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for i in 1..=4 {
        std::fs::write(p.join("f.txt"), format!("{i}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{i}"), "--no-verify"], p),
            "commit",
        );
    }
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let head = rev("HEAD");
    let h1 = rev("HEAD~1");
    let boundary_commit = rev("HEAD~2"); // parent of HEAD~1, excluded by the range

    let out = run_libra_command(&["rev-list", "--boundary", "HEAD~2..HEAD"], p);
    assert_cli_success(&out, "rev-list --boundary HEAD~2..HEAD");
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();

    // Included commits (HEAD, HEAD~1) are listed plain; the boundary is prefixed `-`.
    assert!(lines.contains(&head), "HEAD is listed: {lines:?}");
    assert!(lines.contains(&h1), "HEAD~1 is listed: {lines:?}");
    assert!(
        lines.contains(&format!("-{boundary_commit}")),
        "the excluded parent HEAD~2 is listed as a boundary commit (prefixed -): {lines:?}"
    );
    // The boundary line comes after the included commits and carries no plain entry.
    assert!(
        !lines.contains(&boundary_commit),
        "the boundary commit is not also listed unprefixed: {lines:?}"
    );

    // Without an exclusion there is no boundary.
    let no_range = run_libra_command(&["rev-list", "--boundary", "HEAD"], p);
    assert_cli_success(&no_range, "rev-list --boundary HEAD");
    assert!(
        !String::from_utf8_lossy(&no_range.stdout)
            .lines()
            .any(|l| l.starts_with('-')),
        "no boundary commits without an exclusion"
    );

    // `--count` includes the boundary commits (matching git): 2 listed + 1 boundary.
    let count = run_libra_command(&["rev-list", "--count", "--boundary", "HEAD~2..HEAD"], p);
    assert_cli_success(&count, "rev-list --count --boundary");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        "3",
        "count includes the boundary commit (2 listed + 1 boundary)"
    );
    // Without --boundary the count is the interesting commits only.
    let plain_count = run_libra_command(&["rev-list", "--count", "HEAD~2..HEAD"], p);
    assert_eq!(String::from_utf8_lossy(&plain_count.stdout).trim(), "2");

    // `--max-count` cuts the walk: the boundary is the parent of the LAST listed
    // commit (HEAD~1), not the range start — matching git's "parents of returned
    // commits not themselves returned" rule.
    let limited = run_libra_command(
        &["rev-list", "--boundary", "--max-count=1", "HEAD~2..HEAD"],
        p,
    );
    assert_cli_success(&limited, "rev-list --boundary --max-count=1");
    let limited_lines: Vec<String> = String::from_utf8_lossy(&limited.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert!(
        limited_lines.contains(&head),
        "HEAD listed: {limited_lines:?}"
    );
    assert!(
        limited_lines.contains(&format!("-{h1}")),
        "boundary is the cut-point parent HEAD~1, not the range start: {limited_lines:?}"
    );
    assert!(
        !limited_lines
            .iter()
            .any(|l| l == &format!("-{boundary_commit}")),
        "the range start HEAD~2 is not the boundary under --max-count: {limited_lines:?}"
    );

    // Boundary commits flow through the same formatter: `--timestamp` prefixes the
    // boundary line with its committer timestamp before the `-`-marked id.
    let ts = run_libra_command(
        &["rev-list", "--boundary", "--timestamp", "HEAD~2..HEAD"],
        p,
    );
    assert_cli_success(&ts, "rev-list --boundary --timestamp");
    assert!(
        String::from_utf8_lossy(&ts.stdout).lines().any(|l| l
            .ends_with(&format!(" -{boundary_commit}"))
            && l.starts_with(char::is_numeric)),
        "boundary carries the committer timestamp before the -id: {}",
        String::from_utf8_lossy(&ts.stdout)
    );

    // `--reverse` reverses the COMPLETE stream (listed ++ boundary), so the boundary
    // row leads — matching git rev-list --reverse --boundary.
    let reversed = run_libra_command(&["rev-list", "--reverse", "--boundary", "HEAD~2..HEAD"], p);
    assert_cli_success(&reversed, "rev-list --reverse --boundary");
    let reversed_lines: Vec<String> = String::from_utf8_lossy(&reversed.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(
        reversed_lines.first().map(String::as_str),
        Some(format!("-{boundary_commit}").as_str()),
        "the boundary row leads under --reverse: {reversed_lines:?}"
    );
    assert_eq!(
        reversed_lines.last().map(String::as_str),
        Some(head.as_str()),
        "HEAD is emitted last under --reverse: {reversed_lines:?}"
    );

    // `--children`: the boundary commit carries its children (the listed commits that
    // name it as a parent), even though it is itself excluded from the output set.
    let children = run_libra_command(&["rev-list", "--boundary", "--children", "HEAD~2..HEAD"], p);
    assert_cli_success(&children, "rev-list --boundary --children");
    assert!(
        String::from_utf8_lossy(&children.stdout)
            .lines()
            .any(|l| l == format!("-{boundary_commit} {h1}")),
        "boundary row lists its child HEAD~1: {}",
        String::from_utf8_lossy(&children.stdout)
    );
}

#[test]
fn test_rev_list_boundary_merge_metadata() {
    // Boundary behavior on a merge range, matching git:
    //  - `--first-parent --boundary --parents` rewrites away the parents of the
    //    un-walked second-parent boundary (bare `-id`), while the first-parent-chain
    //    boundary keeps its real parent.
    //  - `--boundary --children` surfaces a boundary commit's children even though it
    //    is itself excluded from the output.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let run = |args: &[&str], ctx: &str| {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, ctx);
    };

    // c1 -> c2 on main; feat branches at c1 with f1; merge feat into main => m.
    std::fs::write(p.join("f.txt"), "1\n").unwrap();
    run(&["add", "f.txt"], "add c1");
    run(&["commit", "-m", "c1", "--no-verify"], "commit c1");
    let c1 = rev("HEAD");
    std::fs::write(p.join("f.txt"), "2\n").unwrap();
    run(&["add", "f.txt"], "add c2");
    run(&["commit", "-m", "c2", "--no-verify"], "commit c2");
    let c2 = rev("HEAD");
    run(&["branch", "feat", &c1], "branch feat");
    run(&["switch", "feat"], "switch feat");
    std::fs::write(p.join("g.txt"), "f\n").unwrap();
    run(&["add", "g.txt"], "add f1");
    run(&["commit", "-m", "f1", "--no-verify"], "commit f1");
    let f1 = rev("HEAD");
    // Return to the default branch and merge feat (creating a merge commit).
    let default_branch = if run_libra_command(&["switch", "main"], p).status.success() {
        "main"
    } else {
        run(&["switch", "master"], "switch master");
        "master"
    };
    let _ = default_branch;
    run(&["merge", "feat", "--no-edit"], "merge feat");
    let m = rev("HEAD");
    // Only proceed if a real merge commit formed (two parents).
    let parents = run_libra_command(&["rev-list", "--parents", "--max-count=1", &m], p);
    let parent_line = String::from_utf8_lossy(&parents.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(
        parent_line.contains(&c2) && parent_line.contains(&f1),
        "merge commit should have c2 and f1 as parents: {parent_line}"
    );

    // `--first-parent --boundary --parents c2..m`: f1 is an un-walked second parent →
    // its boundary row is bare; c2 is the first-parent-chain boundary → shows c1.
    let fp = run_libra_command(
        &[
            "rev-list",
            "--first-parent",
            "--boundary",
            "--parents",
            &format!("{c2}..{m}"),
        ],
        p,
    );
    assert_cli_success(&fp, "first-parent boundary parents");
    let fp_lines: Vec<String> = String::from_utf8_lossy(&fp.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert!(
        fp_lines.iter().any(|l| l == &format!("-{f1}")),
        "un-walked second-parent boundary f1 is bare (no parents): {fp_lines:?}"
    );
    assert!(
        fp_lines.iter().any(|l| l == &format!("-{c2} {c1}")),
        "first-parent-chain boundary c2 keeps its real parent c1: {fp_lines:?}"
    );

    // `--boundary --children c2..m`: the boundary c2 surfaces its child m.
    let ch = run_libra_command(
        &[
            "rev-list",
            "--boundary",
            "--children",
            &format!("{c2}..{m}"),
        ],
        p,
    );
    assert_cli_success(&ch, "boundary children");
    assert!(
        String::from_utf8_lossy(&ch.stdout)
            .lines()
            .any(|l| l.starts_with(&format!("-{c2}")) && l.contains(&m)),
        "boundary c2 lists its child m: {}",
        String::from_utf8_lossy(&ch.stdout)
    );

    // Regression: `--reverse` must NOT reorder a boundary's child list — it only
    // reverses output ROWS. Using `c1..m`, the boundary `-c1` has two children
    // (c2 and f1). The `-c1 …` child line must be byte-identical with and without
    // `--reverse` (this fails if boundary children are computed after the reverse).
    let boundary_child_line = |reverse: bool| -> String {
        let mut argv = vec!["rev-list"];
        if reverse {
            argv.push("--reverse");
        }
        argv.extend_from_slice(&["--boundary", "--children"]);
        let range = format!("{c1}..{m}");
        argv.push(&range);
        let out = run_libra_command(&argv, p);
        assert_cli_success(&out, "boundary children child-order");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.starts_with(&format!("-{c1}")))
            .unwrap_or_default()
            .to_string()
    };
    let forward = boundary_child_line(false);
    let reversed = boundary_child_line(true);
    assert!(
        forward.contains(&c2) && forward.contains(&f1),
        "boundary c1 lists both children: {forward}"
    );
    assert_eq!(
        forward, reversed,
        "the boundary child list is identical with and without --reverse (only rows reverse)"
    );
}

/// `rev-list --objects HEAD` lists the commit, then the deduplicated reachable
/// tree/blob objects: the root tree as `<oid> ` (empty path, trailing space) and
/// each blob as `<oid> <path>`. Matches `git rev-list --objects` formatting.
#[test]
fn test_rev_list_objects_lists_tree_and_blobs() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["rev-list", "--objects", "HEAD"], repo.path());
    assert_cli_success(&out, "rev-list --objects HEAD");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();

    // First line is the commit oid with NO trailing space / path.
    assert!(
        !lines[0].contains(' ') && lines[0].len() == 40,
        "first line is the bare commit oid: {:?}",
        lines[0]
    );

    // The root tree line is `<oid> ` — 40-hex oid, a space, then an empty path.
    assert!(
        lines.iter().any(|l| {
            l.len() == 41 && l.ends_with(' ') && l[..40].chars().all(|c| c.is_ascii_hexdigit())
        }),
        "root tree printed as `<oid> ` with empty path: {lines:?}"
    );

    // The committed file appears as a blob line `<oid> tracked.txt`.
    assert!(
        lines.iter().any(|l| l.ends_with(" tracked.txt")),
        "tracked.txt blob is listed with its path: {lines:?}"
    );

    // Object ids are deduplicated (no oid repeated across object lines).
    let object_oids: Vec<&str> = lines[1..].iter().map(|l| &l[..40]).collect();
    let mut sorted = object_oids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        object_oids.len(),
        "object ids are deduplicated: {object_oids:?}"
    );
}

/// `rev-list --json --objects` carries the objects in a structured `objects`
/// array of `{ oid, path }`; the array is omitted when `--objects` is not given.
#[test]
fn test_rev_list_objects_json_contract() {
    let repo = create_committed_repo_via_cli();

    let with = run_libra_command(&["--json", "rev-list", "--objects", "HEAD"], repo.path());
    assert_cli_success(&with, "rev-list --json --objects");
    let json: serde_json::Value = serde_json::from_slice(&with.stdout).expect("valid JSON");
    let objects = json["data"]["objects"].as_array().expect("objects array");
    assert!(
        objects.iter().any(|o| o["path"] == "tracked.txt"),
        "objects array names tracked.txt: {json}"
    );
    assert!(
        objects
            .iter()
            .any(|o| o["path"] == "" && o["oid"].as_str().is_some_and(|s| s.len() == 40)),
        "objects array includes the root tree with an empty path: {json}"
    );

    // Without --objects the field is omitted.
    let without = run_libra_command(&["--json", "rev-list", "HEAD"], repo.path());
    assert_cli_success(&without, "rev-list --json");
    let json: serde_json::Value = serde_json::from_slice(&without.stdout).expect("valid JSON");
    assert!(
        json["data"].get("objects").is_none(),
        "objects omitted without --objects: {json}"
    );
}

/// `--objects-edge` implies `--objects` and additionally prints the excluded
/// boundary commit(s) with a `-` prefix (edge markers for pack builders).
#[test]
fn test_rev_list_objects_edge_marks_boundary() {
    let repo = create_committed_repo_via_cli();
    // Add a second commit so HEAD~1 is an excludable boundary.
    std::fs::write(repo.path().join("more.txt"), "more\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "more.txt"], repo.path()),
        "add more.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], repo.path()),
        "second commit",
    );

    let out = run_libra_command(&["rev-list", "--objects-edge", "HEAD~1..HEAD"], repo.path());
    assert_cli_success(&out, "rev-list --objects-edge HEAD~1..HEAD");
    let stdout = String::from_utf8(out.stdout).unwrap();
    // The excluded boundary commit (HEAD~1) is printed with a `-` prefix, and the
    // objects of the included commit are still listed (--objects implied).
    assert!(
        stdout.lines().any(|l| l.starts_with('-') && l.len() == 41),
        "boundary edge commit printed with `-` prefix: {stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.ends_with(" more.txt")),
        "objects of the included commit are listed: {stdout}"
    );
}

/// `rev-list --objects A..B` emits only objects new to the included side: trees
/// and blobs shared with the excluded commit `A` are pre-marked uninteresting and
/// omitted (Git's object-closure semantics).
#[test]
fn test_rev_list_objects_excludes_unchanged_from_range() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::write(p.join("src/main.rs"), "a\n").unwrap();
    std::fs::write(p.join("docs/readme.md"), "doc\n").unwrap();
    std::fs::write(p.join("top.txt"), "t\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    std::fs::write(p.join("src/main.rs"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "src/main.rs"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let out = run_libra_command(&["rev-list", "--objects", "HEAD~1..HEAD"], p);
    assert_cli_success(&out, "rev-list --objects HEAD~1..HEAD");
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Only the changed src subtree + blob appear; unchanged docs/top.txt/lib do not.
    assert!(
        stdout.contains(" src/main.rs"),
        "changed blob listed: {stdout}"
    );
    assert!(
        stdout.contains(" src\n"),
        "changed subtree listed: {stdout}"
    );
    assert!(
        !stdout.contains(" docs") && !stdout.contains(" top.txt"),
        "objects shared with the excluded commit are omitted: {stdout}"
    );
}

/// `rev-list --objects HEAD -- <pathspec>` prunes the object walk: only the root
/// tree, the matched subtree, and its contents are listed; unrelated subtrees and
/// blobs are dropped.
#[test]
fn test_rev_list_objects_pathspec_prunes_walk() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::write(p.join("src/main.rs"), "a\n").unwrap();
    std::fs::write(p.join("docs/readme.md"), "doc\n").unwrap();
    std::fs::write(p.join("top.txt"), "t\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );

    let out = run_libra_command(&["rev-list", "--objects", "HEAD", "--", "src"], p);
    assert_cli_success(&out, "rev-list --objects HEAD -- src");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(" src/main.rs"),
        "matched blob listed: {stdout}"
    );
    assert!(
        stdout.contains(" src\n"),
        "matched subtree listed: {stdout}"
    );
    assert!(
        !stdout.contains("docs") && !stdout.contains("top.txt"),
        "unrelated subtrees/blobs are pruned by the pathspec: {stdout}"
    );
}

/// `--count --objects` includes the enumerated objects in the count, so it equals
/// the number of lines `--objects` would print (commits + objects).
#[test]
fn test_rev_list_count_objects_includes_objects() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let listed = run_libra_command(&["rev-list", "--objects", "HEAD"], p);
    assert_cli_success(&listed, "rev-list --objects");
    let line_count = String::from_utf8(listed.stdout).unwrap().lines().count();

    let counted = run_libra_command(&["rev-list", "--count", "--objects", "HEAD"], p);
    assert_cli_success(&counted, "rev-list --count --objects");
    let count: usize = String::from_utf8(counted.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("count is a number");
    assert_eq!(
        count, line_count,
        "--count --objects counts commits + objects"
    );
}

/// `rev-list --objects` skips gitlink (`160000`/`TreeItemMode::Commit`) tree
/// entries — Git omits submodule pointers from object enumeration — while still
/// listing the sibling blob.
#[test]
#[serial(hash_kind)]
fn test_rev_list_objects_skips_gitlinks() {
    use git_internal::internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    };
    use libra::utils::client_storage::ClientStorage;

    let repo = tempfile::tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let _kind = set_hash_kind_for_test(HashKind::Sha1);
    let storage = ClientStorage::init(repo.path().join(".libra/objects"));

    let blob_oid = ObjectHash::from_bytes(&[0x11; 20]).unwrap();
    let gitlink_oid = ObjectHash::from_bytes(&[0x22; 20]).unwrap();
    let tree = Tree::from_tree_items(vec![
        TreeItem {
            mode: TreeItemMode::Blob,
            id: blob_oid,
            name: "file.txt".to_string(),
        },
        TreeItem {
            mode: TreeItemMode::Commit,
            id: gitlink_oid,
            name: "submodule".to_string(),
        },
    ])
    .expect("build tree");
    storage
        .put(&tree.id, &tree.to_data().unwrap(), ObjectType::Tree)
        .unwrap();
    let sig = Signature {
        signature_type: SignatureType::Author,
        name: "t".to_string(),
        email: "t@t".to_string(),
        timestamp: 1_700_000_000,
        timezone: "+0000".to_string(),
    };
    let mut committer = sig.clone();
    committer.signature_type = SignatureType::Committer;
    let commit = Commit::new(sig, committer, tree.id, vec![], "gitlink fixture");
    storage
        .put(&commit.id, &commit.to_data().unwrap(), ObjectType::Commit)
        .unwrap();

    let out = run_libra_command(
        &["rev-list", "--objects", &commit.id.to_string()],
        repo.path(),
    );
    assert_cli_success(&out, "rev-list --objects on a tree with a gitlink");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(&blob_oid.to_string()),
        "the sibling blob is listed: {stdout}"
    );
    assert!(
        !stdout.contains(&gitlink_oid.to_string()),
        "the gitlink/submodule pointer is omitted: {stdout}"
    );
}

/// `rev-list --objects` fails (does not silently truncate) when an included
/// commit's tree references a subtree that is missing/corrupt — an
/// object-enumeration plumbing command must not emit an incomplete closure.
#[test]
#[serial(hash_kind)]
fn test_rev_list_objects_errors_on_corrupt_subtree() {
    use git_internal::internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    };
    use libra::utils::client_storage::ClientStorage;

    let repo = tempfile::tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let _kind = set_hash_kind_for_test(HashKind::Sha1);
    let storage = ClientStorage::init(repo.path().join(".libra/objects"));

    // Root tree points at a subtree that is never stored.
    let missing_subtree = ObjectHash::from_bytes(&[0x33; 20]).unwrap();
    let tree = Tree::from_tree_items(vec![TreeItem {
        mode: TreeItemMode::Tree,
        id: missing_subtree,
        name: "sub".to_string(),
    }])
    .expect("build tree");
    storage
        .put(&tree.id, &tree.to_data().unwrap(), ObjectType::Tree)
        .unwrap();
    let sig = Signature {
        signature_type: SignatureType::Author,
        name: "t".to_string(),
        email: "t@t".to_string(),
        timestamp: 1_700_000_000,
        timezone: "+0000".to_string(),
    };
    let mut committer = sig.clone();
    committer.signature_type = SignatureType::Committer;
    let commit = Commit::new(sig, committer, tree.id, vec![], "corrupt fixture");
    storage
        .put(&commit.id, &commit.to_data().unwrap(), ObjectType::Commit)
        .unwrap();

    let out = run_libra_command(
        &["rev-list", "--objects", &commit.id.to_string()],
        repo.path(),
    );
    assert!(
        !out.status.success(),
        "rev-list --objects must fail on a missing subtree rather than truncate"
    );
}

/// A parent omitted by `--max-count`/`-n` is NOT "excluded" — its shared objects
/// must still be listed. (Regression: `--objects -n 1 HEAD` lists HEAD's full
/// object closure, not just objects new since its parent.)
#[test]
fn test_rev_list_objects_n1_does_not_suppress_parent_objects() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("keep.txt"), "keep\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "keep.txt"], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    std::fs::write(p.join("new.txt"), "new\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.txt"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let out = run_libra_command(&["rev-list", "-n", "1", "--objects", "HEAD"], p);
    assert_cli_success(&out, "rev-list -n 1 --objects HEAD");
    let stdout = String::from_utf8(out.stdout).unwrap();
    // keep.txt is unchanged since the (omitted-by -n1, not excluded) parent, yet
    // it MUST still be listed — only `^`/range exclusions suppress objects.
    assert!(
        stdout.contains(" keep.txt"),
        "parent-shared blob still listed: {stdout}"
    );
    assert!(stdout.contains(" new.txt"), "new blob listed: {stdout}");
}

/// Regression: with a subtree object shared by two paths and mixed narrow/broad
/// pathspecs, the broad path must still contribute its objects. Dedup of emitted
/// objects must not prune a later, broader traversal of the same tree.
#[test]
fn test_rev_list_objects_shared_subtree_mixed_pathspecs() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    // a/ and b/ are byte-identical directories (so they share one tree object),
    // but x and y differ (two distinct blobs).
    std::fs::create_dir_all(p.join("a")).unwrap();
    std::fs::create_dir_all(p.join("b")).unwrap();
    std::fs::write(p.join("a/x"), "XXX").unwrap();
    std::fs::write(p.join("a/y"), "YYY").unwrap();
    std::fs::write(p.join("b/x"), "XXX").unwrap();
    std::fs::write(p.join("b/y"), "YYY").unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );

    // Narrow `a/x` + broad `b`: a/x and b/y are distinct blobs and BOTH must be
    // listed (the broad `b` traversal must not be pruned by the narrow `a/x` one).
    let out = run_libra_command(&["rev-list", "--objects", "HEAD", "--", "a/x", "b"], p);
    assert_cli_success(&out, "rev-list --objects HEAD -- a/x b");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(" a/x"),
        "narrow pathspec blob listed: {stdout}"
    );
    assert!(
        stdout.contains(" b/y"),
        "broad pathspec's distinct blob is NOT dropped by shared-subtree dedup: {stdout}"
    );
}

/// `--count --objects` cannot be combined with the marked count modes
/// (`--left-right`/`--cherry-mark`/`--cherry`), since objects carry no side; the
/// combination is rejected rather than reporting an inflated first count field.
#[test]
fn test_rev_list_count_objects_rejects_marked_modes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for marked in [["--left-right"], ["--cherry-mark"], ["--cherry"]] {
        let mut argv = vec!["rev-list", "--count", "--objects"];
        argv.extend_from_slice(&marked);
        argv.push("HEAD");
        let out = run_libra_command(&argv, p);
        assert!(
            !out.status.success(),
            "--count --objects {marked:?} must be rejected: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// `--count --objects-edge` counts commits + objects but NOT the `-`-prefixed
/// edge commits (which are computed only as pack-builder edge markers, not a
/// user `--boundary`). The count therefore equals the plain `--objects` line
/// count, while `--objects-edge` itself prints the extra edge line.
#[test]
fn test_rev_list_count_objects_edge_excludes_edge_commits() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let object_lines =
        String::from_utf8(run_libra_command(&["rev-list", "--objects", "HEAD~1..HEAD"], p).stdout)
            .unwrap()
            .lines()
            .count();
    let edge_count: usize = String::from_utf8(
        run_libra_command(
            &["rev-list", "--count", "--objects-edge", "HEAD~1..HEAD"],
            p,
        )
        .stdout,
    )
    .unwrap()
    .trim()
    .parse()
    .expect("count is numeric");
    assert_eq!(
        edge_count, object_lines,
        "--count --objects-edge counts commits + objects, not the edge commits"
    );
}

fn two_commit_shallow_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("second.txt"), "second\n").expect("write second");
    assert_cli_success(
        &run_libra_command(&["add", "second.txt"], repo.path()),
        "add second",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "second commit",
    );
    let log = run_libra_command(&["log", "--pretty=%H"], repo.path());
    assert_cli_success(&log, "log hashes");
    let child = String::from_utf8_lossy(&log.stdout)
        .lines()
        .next()
        .expect("HEAD")
        .trim()
        .to_string();
    let parent = String::from_utf8_lossy(&log.stdout)
        .lines()
        .nth(1)
        .expect("parent")
        .trim()
        .to_string();
    std::fs::remove_file(loose_object_path(repo.path(), &parent)).expect("delete parent");
    std::fs::write(
        repo.path().join(".libra").join("shallow"),
        format!("{child}\n"),
    )
    .expect("write shallow");
    repo
}

/// M-WALK W2: rev-list counts only the shallow-reachable history.
#[test]
fn test_rev_list_counts_shallow_history() {
    let repo = two_commit_shallow_repo();
    for args in [
        &["rev-list", "--count", "HEAD"][..],
        &["rev-list", "--all", "--count"][..],
    ] {
        let output = run_libra_command(args, repo.path());
        assert_cli_success(&output, &args.join(" "));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "1",
            "{args:?} must count only the boundary commit"
        );
    }
}

// --- BRL-04: date-priority walker with max-count early stop (G1–G10) ---

fn brl04_fresh_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    repo
}

fn brl04_commit_at(
    repo: &std::path::Path,
    path: &str,
    body: &str,
    message: &str,
    committer: u64,
    author: u64,
) {
    std::fs::write(repo.join(path), body).expect("write file");
    assert_cli_success(&run_libra_command(&["add", path], repo), "add");
    let committer_date = format!("{committer} +0000");
    let author_date = format!("{author} +0000");
    assert_cli_success(
        &run_libra_command_with_env(
            &["commit", "-m", message, "--no-verify"],
            repo,
            &[
                ("GIT_COMMITTER_DATE", committer_date.as_str()),
                ("GIT_AUTHOR_DATE", author_date.as_str()),
            ],
        ),
        "commit",
    );
}

fn brl04_rev_list_count(repo: &std::path::Path, specs: &[&str]) -> u64 {
    let mut args = vec!["rev-list", "--count"];
    args.extend_from_slice(specs);
    let output = run_libra_command(&args, repo);
    assert_cli_success(&output, "rev-list --count");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("rev-list count")
}

fn brl04_with_load_count(repo: &std::path::Path, args: &[&str]) -> (std::process::Output, u64) {
    let loaded_path = repo.join(".commits-loaded");
    let _ = std::fs::remove_file(&loaded_path);
    let output = run_libra_command_with_env(
        args,
        repo,
        &[(
            "LIBRA_TEST_LOG_COMMITS_LOADED_PATH",
            loaded_path.to_str().expect("loaded path"),
        )],
    );
    assert_cli_success(&output, &args.join(" "));
    let loaded = std::fs::read_to_string(&loaded_path)
        .expect("load counter")
        .trim()
        .parse()
        .expect("load counter integer");
    (output, loaded)
}

fn brl04_lines(repo: &std::path::Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["rev-list"];
    args.extend_from_slice(extra);
    let output = run_libra_command(&args, repo);
    assert_cli_success(&output, &args.join(" "));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn brl04_linear_history(n: usize) -> tempfile::TempDir {
    let repo = brl04_fresh_repo();
    for i in 1..=n {
        brl04_commit_at(
            repo.path(),
            "f.txt",
            &format!("v{i}\n"),
            &format!("m{i}"),
            1_900_000_000 + i as u64,
            1_900_000_000 + i as u64,
        );
    }
    assert_eq!(brl04_rev_list_count(repo.path(), &["HEAD"]), n as u64);
    repo
}

/// Clock-skewed history from ADR-BRL-02: M(10), P2(9), P1(5), B(5), A(5).
fn brl04_skew_repo() -> tempfile::TempDir {
    let repo = brl04_fresh_repo();
    brl04_commit_at(
        repo.path(),
        "a.txt",
        "a\n",
        "A",
        1_920_000_005,
        1_920_000_005,
    );
    assert_cli_success(
        &run_libra_command(&["switch", "--orphan", "side"], repo.path()),
        "orphan side",
    );
    brl04_commit_at(
        repo.path(),
        "b.txt",
        "b\n",
        "B",
        1_920_000_005,
        1_920_000_005,
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "switch main",
    );
    brl04_commit_at(
        repo.path(),
        "p1.txt",
        "p1\n",
        "P1",
        1_920_000_005,
        1_920_000_005,
    );
    assert_cli_success(
        &run_libra_command(&["switch", "side"], repo.path()),
        "switch side",
    );
    brl04_commit_at(
        repo.path(),
        "p2.txt",
        "p2\n",
        "P2",
        1_920_000_009,
        1_920_000_009,
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "switch main for merge",
    );
    let stamp = "1920000010 +0000";
    assert_cli_success(
        &run_libra_command_with_env(
            &[
                "merge",
                "side",
                "-m",
                "M",
                "--no-edit",
                "--no-verify",
                "--allow-unrelated-histories",
            ],
            repo.path(),
            &[("GIT_COMMITTER_DATE", stamp), ("GIT_AUTHOR_DATE", stamp)],
        ),
        "merge unrelated side",
    );
    repo
}

fn brl04_subjects_from_hashes(repo: &std::path::Path, hashes: &[String]) -> Vec<String> {
    hashes
        .iter()
        .map(|hash| {
            let output = run_libra_command(&["log", "-1", "--format=%s", hash], repo);
            assert_cli_success(&output, "subject for hash");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        })
        .collect()
}

/// Deep merges under a long non-merge tip chain: early-stop would miss merges.
fn brl04_merges_under_linear_tip() -> tempfile::TempDir {
    let repo = brl04_fresh_repo();
    brl04_commit_at(
        repo.path(),
        "base.txt",
        "base\n",
        "base",
        1_930_000_001,
        1_930_000_001,
    );
    for i in 1..=3 {
        let branch = format!("side{i}");
        assert_cli_success(
            &run_libra_command(&["switch", "-c", &branch], repo.path()),
            "create side",
        );
        brl04_commit_at(
            repo.path(),
            &format!("s{i}.txt"),
            &format!("s{i}\n"),
            &format!("S{i}"),
            1_930_000_010 + i as u64,
            1_930_000_010 + i as u64,
        );
        assert_cli_success(
            &run_libra_command(&["switch", "main"], repo.path()),
            "back main",
        );
        // Diverging commit on main so the merge is not a fast-forward.
        brl04_commit_at(
            repo.path(),
            &format!("m{i}.txt"),
            &format!("m{i}\n"),
            &format!("Main{i}"),
            1_930_000_015 + i as u64,
            1_930_000_015 + i as u64,
        );
        let stamp = format!("{} +0000", 1_930_000_020 + i);
        assert_cli_success(
            &run_libra_command_with_env(
                &[
                    "merge",
                    &branch,
                    "-m",
                    &format!("Merge{i}"),
                    "--no-edit",
                    "--no-verify",
                    "--no-ff",
                ],
                repo.path(),
                &[
                    ("GIT_COMMITTER_DATE", stamp.as_str()),
                    ("GIT_AUTHOR_DATE", stamp.as_str()),
                ],
            ),
            "merge side",
        );
    }
    // Ten non-merge commits on tip so -n 5 never reaches the merges if early-stopped.
    for i in 1..=10 {
        brl04_commit_at(
            repo.path(),
            "tip.txt",
            &format!("tip{i}\n"),
            &format!("tip{i}"),
            1_930_000_100 + i as u64,
            1_930_000_100 + i as u64,
        );
    }
    repo
}

#[test]
fn rev_list_max_count_loads_bounded_commits() {
    let repo = brl04_linear_history(8);
    let (_output, loaded) = brl04_with_load_count(repo.path(), &["rev-list", "-n", "1", "HEAD"]);
    assert_eq!(loaded, 2, "tip plus the parent enqueued on the frontier");
    assert!(loaded < brl04_rev_list_count(repo.path(), &["HEAD"]));
}

#[test]
fn rev_list_max_count_with_skip_loads_bounded_commits() {
    let repo = brl04_linear_history(8);
    let (_output, loaded) =
        brl04_with_load_count(repo.path(), &["rev-list", "-n", "3", "--skip", "2", "HEAD"]);
    assert_eq!(loaded, 6, "skip 2 + 3 yields, plus the next parent");
    assert_eq!(brl04_rev_list_count(repo.path(), &["HEAD"]), 8);
}

#[test]
fn rev_list_limited_output_is_prefix_of_unlimited() {
    let repo = brl04_linear_history(8);
    let full = brl04_lines(repo.path(), &["HEAD"]);
    assert_eq!(full.len(), 8);
    for n in [1usize, 5, full.len()] {
        let limited = brl04_lines(repo.path(), &["-n", &n.to_string(), "HEAD"]);
        assert_eq!(limited, full[..n]);
    }
    // Unlimited eligible path also uses the walker (date-order no-op).
    let dated = brl04_lines(repo.path(), &["--date-order", "HEAD"]);
    assert_eq!(dated, full);
}

#[test]
fn rev_list_clock_skew_uses_date_priority_order() {
    let repo = brl04_skew_repo();
    let hashes = brl04_lines(repo.path(), &["HEAD"]);
    assert_eq!(
        brl04_subjects_from_hashes(repo.path(), &hashes),
        vec!["M", "P2", "P1", "B", "A"]
    );
}

#[test]
fn rev_list_clock_skew_limited_is_prefix() {
    let repo = brl04_skew_repo();
    let full = brl04_lines(repo.path(), &["HEAD"]);
    for n in [1usize, 3] {
        let limited = brl04_lines(repo.path(), &["-n", &n.to_string(), "HEAD"]);
        assert_eq!(limited, full[..n]);
    }
}

#[test]
fn rev_list_first_parent_limited_is_prefix() {
    let repo = brl04_skew_repo();
    let full = brl04_lines(repo.path(), &["--first-parent", "HEAD"]);
    assert!(!full.is_empty());
    for n in [1usize, full.len().min(2)] {
        let limited = brl04_lines(
            repo.path(),
            &["--first-parent", "-n", &n.to_string(), "HEAD"],
        );
        assert_eq!(limited, full[..n]);
    }
}

#[test]
fn rev_list_json_machine_limited_is_prefix() {
    let repo = brl04_linear_history(6);
    let unlimited_json = run_libra_command(&["--json", "rev-list", "HEAD"], repo.path());
    assert_cli_success(&unlimited_json, "json rev-list");
    let all = parse_json_stdout(&unlimited_json)["data"]["commits"]
        .as_array()
        .expect("commits")
        .clone();
    let n = 2;
    let limited_json = run_libra_command(
        &["--json", "rev-list", "-n", &n.to_string(), "HEAD"],
        repo.path(),
    );
    assert_cli_success(&limited_json, "json rev-list -n");
    let limited_json_value = parse_json_stdout(&limited_json);
    let head = limited_json_value["data"]["commits"]
        .as_array()
        .expect("limited commits");
    assert_eq!(head.len(), n);
    assert_eq!(head.as_slice(), &all[..n]);

    let unlimited_machine = run_libra_command(&["--machine", "rev-list", "HEAD"], repo.path());
    assert_cli_success(&unlimited_machine, "machine rev-list");
    let all_m = parse_json_stdout(&unlimited_machine)["data"]["commits"]
        .as_array()
        .expect("commits")
        .clone();
    let limited_machine = run_libra_command(
        &["--machine", "rev-list", "-n", &n.to_string(), "HEAD"],
        repo.path(),
    );
    assert_cli_success(&limited_machine, "machine rev-list -n");
    let limited_machine_value = parse_json_stdout(&limited_machine);
    let head_m = limited_machine_value["data"]["commits"]
        .as_array()
        .expect("limited commits");
    assert_eq!(head_m.len(), n);
    assert_eq!(head_m.as_slice(), &all_m[..n]);
}

#[test]
fn rev_list_merges_filter_with_max_count_is_ineligible() {
    let repo = brl04_merges_under_linear_tip();
    let merge_total = brl04_lines(repo.path(), &["--merges", "HEAD"]).len();
    assert!(merge_total >= 3, "fixture needs several merges");

    // Filter-then-limit: deep merges still appear. Early-stop-then-filter would
    // yield an empty list because the tip chain is non-merge.
    let limited = brl04_lines(repo.path(), &["--merges", "-n", "2", "HEAD"]);
    assert_eq!(
        limited.len(),
        2,
        "must still find merges beyond the tip window"
    );

    // Table-driven: each filter-chain representative still returns results under -n
    // when matches exist deeper than an early-stop window.
    for args in [
        &["rev-list", "--no-merges", "-n", "5", "HEAD"][..],
        &["rev-list", "--min-parents", "1", "-n", "5", "HEAD"][..],
        &["rev-list", "--max-parents", "2", "-n", "5", "HEAD"][..],
        &["rev-list", "--no-min-parents", "-n", "5", "HEAD"][..],
        &["rev-list", "--no-max-parents", "-n", "5", "HEAD"][..],
        &["rev-list", "--author", "Test", "-n", "5", "HEAD"][..],
        &["rev-list", "--committer", "Test", "-n", "5", "HEAD"][..],
        &["rev-list", "--grep", "tip", "-n", "5", "HEAD"][..],
        &["rev-list", "--since", "2000-01-01", "-n", "5", "HEAD"][..],
        &["rev-list", "--until", "2099-01-01", "-n", "5", "HEAD"][..],
    ] {
        let output = run_libra_command(args, repo.path());
        assert_cli_success(&output, &args.join(" "));
        let lines = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        assert_eq!(lines, 5, "{args:?} must keep filter-then-limit semantics");
    }
}

#[test]
fn rev_list_objects_edge_flags_stay_on_full_path() {
    let repo = brl04_linear_history(4);
    for flag in ["--objects-edge", "--objects-edge-aggressive"] {
        let unlimited = run_libra_command(&["rev-list", flag, "HEAD"], repo.path());
        assert_cli_success(&unlimited, flag);
        let unlimited_text = String::from_utf8_lossy(&unlimited.stdout);
        assert!(
            unlimited_text.contains(' '),
            "{flag} must enumerate objects: {unlimited_text}"
        );

        let limited = run_libra_command(&["rev-list", flag, "-n", "1", "HEAD"], repo.path());
        assert_cli_success(&limited, &format!("{flag} -n 1"));
        let limited_text = String::from_utf8_lossy(&limited.stdout);
        // Full path: limit applies after collect; edge frontier from the cut still prints.
        let limited_lines: Vec<&str> = limited_text.lines().collect();
        assert!(
            limited_lines.iter().any(|line| line.starts_with('-')),
            "{flag} -n 1 must keep edge markers from the full-path frontier: {limited_text}"
        );
        assert!(
            limited_lines.iter().any(|line| line.contains(' ')),
            "{flag} -n 1 must still list objects: {limited_text}"
        );
    }
}

#[test]
fn rev_list_full_path_options_unchanged() {
    let repo = brl04_linear_history(5);
    let full_count = brl04_rev_list_count(repo.path(), &["HEAD"]);
    assert_eq!(full_count, 5);

    let counted = run_libra_command(&["rev-list", "--count", "-n", "2", "HEAD"], repo.path());
    assert_cli_success(&counted, "--count -n 2");
    assert_eq!(String::from_utf8_lossy(&counted.stdout).trim(), "2");

    let reversed = brl04_lines(repo.path(), &["--reverse", "-n", "2", "HEAD"]);
    let forward = brl04_lines(repo.path(), &["-n", "2", "HEAD"]);
    assert_eq!(
        reversed,
        forward.iter().rev().cloned().collect::<Vec<_>>(),
        "--reverse stays limit-then-reverse on the full path"
    );

    let all = run_libra_command(&["rev-list", "--all", "--count"], repo.path());
    assert_cli_success(&all, "--all --count");
    assert_eq!(
        String::from_utf8_lossy(&all.stdout)
            .trim()
            .parse::<u64>()
            .expect("count"),
        full_count
    );

    let boundary = run_libra_command(&["rev-list", "--boundary", "-n", "1", "HEAD"], repo.path());
    assert_cli_success(&boundary, "--boundary -n 1");
    let boundary_text = String::from_utf8_lossy(&boundary.stdout);
    assert!(
        boundary_text.lines().any(|line| line.starts_with('-')),
        "boundary marker retained: {boundary_text}"
    );

    // Pathspec stays on the full path and still filters.
    let path_limited = brl04_lines(repo.path(), &["HEAD", "--", "f.txt"]);
    assert_eq!(path_limited.len(), 5);

    // left-right needs a symmetric difference; create a second tip.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "other"], repo.path()),
        "branch other",
    );
    brl04_commit_at(
        repo.path(),
        "other.txt",
        "o\n",
        "other",
        1_900_000_050,
        1_900_000_050,
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "back main",
    );
    let left_right = run_libra_command(
        &["rev-list", "--left-right", "-n", "1", "main...other"],
        repo.path(),
    );
    assert_cli_success(&left_right, "--left-right -n 1");
    let lr = String::from_utf8_lossy(&left_right.stdout);
    assert!(
        lr.starts_with('<') || lr.starts_with('>'),
        "left-right prefix retained: {lr}"
    );
}
