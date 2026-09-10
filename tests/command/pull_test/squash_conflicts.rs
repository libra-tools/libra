//! A conflicted squash pulled from a local remote has no merge lifecycle.

use std::{collections::BTreeMap, fs, path::Path, process::Output};

use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, configure_identity_via_cli, configure_pull_tracking, create_remote_fixture,
    init_repo_via_cli, parse_cli_error_stderr, push_remote_commit, run_libra_command,
};

struct ConflictFixture {
    _remote: TempDir,
    local: TempDir,
    upstream: std::path::PathBuf,
    branch: String,
}

fn cli_text(repo: &Path, args: &[&str]) -> String {
    let output = run_libra_command(args, repo);
    assert_cli_success(&output, &format!("{args:?}"));
    String::from_utf8(output.stdout)
        .expect("CLI text")
        .trim()
        .to_owned()
}

fn conflict_fixture() -> ConflictFixture {
    let (remote, remote_dir, upstream, branch) = create_remote_fixture();
    push_remote_commit(&upstream, &branch, "dirty.txt", "clean\n", "clean baseline");
    let local = tempdir().expect("local repo");
    let p = local.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    configure_pull_tracking(p, &remote_dir, &branch);
    cli_text(p, &["pull"]);
    fs::write(p.join("README.md"), "local conflict\n").expect("local edit");
    cli_text(p, &["add", "README.md"]);
    cli_text(p, &["commit", "-m", "local edit", "--no-verify"]);
    push_remote_commit(
        &upstream,
        &branch,
        "README.md",
        "remote conflict\n",
        "remote edit",
    );
    ConflictFixture {
        _remote: remote,
        local,
        upstream,
        branch,
    }
}

fn assert_squash_conflict(output: &Output) {
    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert_eq!(
        report.details.get("phase"),
        Some(&serde_json::json!("merge"))
    );
    let hints = report.hints.join("\n");
    assert!(hints.contains("'libra commit'"), "{hints}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("merge --continue"), "{stderr}");
    assert!(!stderr.contains("merge --abort"), "{stderr}");
}

fn assert_conflict_stages(repo: &Path) {
    let listing = cli_text(repo, &["ls-files", "-s"]);
    let stages: Vec<_> = listing
        .lines()
        .filter(|line| line.ends_with("\tREADME.md"))
        .collect();
    assert_eq!(stages.len(), 3, "{listing}");
    for (stage, content) in [
        (1, "hello libra"),
        (2, "local conflict"),
        (3, "remote conflict"),
    ] {
        let marker = format!(" {stage}\t");
        let line = stages
            .iter()
            .find(|line| line.contains(&marker))
            .expect("conflict stage");
        let oid = line.split_whitespace().nth(1).expect("stage OID");
        assert_eq!(cli_text(repo, &["cat-file", "-p", oid]), content);
    }
}

fn worktree_bytes(repo: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(repo)
        .expect("worktree")
        .map(|entry| {
            let entry = entry.expect("worktree entry");
            (
                entry.file_name().into_string().expect("fixture path"),
                entry.path(),
            )
        })
        .filter(|(name, _)| name != ".libra" && name != ".libra-test-home")
        .map(|(name, path)| (name, fs::read(path).expect("fixture file")))
        .collect()
}

#[test]
fn pull_squash_conflict_uses_plain_commit_without_merge_state() {
    let fixture = conflict_fixture();
    let p = fixture.local.path();
    let head = cli_text(p, &["rev-parse", "HEAD"]);

    assert_squash_conflict(&run_libra_command(&["pull", "--squash"], p));

    assert_eq!(cli_text(p, &["rev-parse", "HEAD"]), head);
    assert!(!p.join(".libra/merge-state.json").exists());
    assert_conflict_stages(p);
    let conflict = fs::read_to_string(p.join("README.md")).expect("conflict markers");
    for marker in [
        "<<<<<<<",
        "=======",
        ">>>>>>>",
        "local conflict",
        "remote conflict",
    ] {
        assert!(conflict.contains(marker), "{conflict}");
    }

    fs::write(p.join("README.md"), "resolved\n").expect("resolve");
    cli_text(p, &["add", "README.md"]);
    cli_text(p, &["commit", "-m", "squashed resolution", "--no-verify"]);
    let commit = cli_text(p, &["cat-file", "-p", "HEAD"]);
    let parents: Vec<_> = commit
        .lines()
        .filter(|line| line.starts_with("parent "))
        .collect();
    assert_eq!(parents, vec![format!("parent {head}")]);
    assert!(!p.join(".libra/merge-state.json").exists());
    assert_eq!(
        fs::read(p.join("README.md")).expect("resolution"),
        b"resolved\n"
    );
}

#[test]
fn pull_after_squash_conflict_refuses_without_changing_resolution_state() {
    let fixture = conflict_fixture();
    let p = fixture.local.path();
    assert_squash_conflict(&run_libra_command(&["pull", "--squash"], p));
    assert_conflict_stages(p);
    fs::write(p.join("keep.txt"), "untracked work\n").expect("untracked work");
    let head = cli_text(p, &["rev-parse", "HEAD"]);
    let index = fs::read(p.join(".libra/index")).expect("index");
    let worktree = worktree_bytes(p);
    // Fetch may advance remote refs, but integration must not replace the index.
    push_remote_commit(
        &fixture.upstream,
        &fixture.branch,
        "fetched-later.txt",
        "new upstream\n",
        "later remote edit",
    );

    assert_squash_conflict(&run_libra_command(&["pull"], p));

    assert_eq!(cli_text(p, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(p.join(".libra/index")).expect("index"), index);
    assert_eq!(worktree_bytes(p), worktree);
    assert!(!p.join(".libra/merge-state.json").exists());
    assert_conflict_stages(p);
}

#[test]
fn pull_squash_conflict_saves_autostash_without_applying_it() {
    let fixture = conflict_fixture();
    let p = fixture.local.path();
    let head = cli_text(p, &["rev-parse", "HEAD"]);
    fs::write(p.join("dirty.txt"), "uncommitted work\n").expect("dirty edit");

    assert_squash_conflict(&run_libra_command(&["pull", "--squash", "--autostash"], p));

    assert_eq!(cli_text(p, &["rev-parse", "HEAD"]), head);
    assert!(!p.join(".libra/merge-state.json").exists());
    assert!(!p.join(".libra/merge-autostash.json").exists());
    assert_conflict_stages(p);
    assert_eq!(
        fs::read(p.join("dirty.txt")).expect("clean file"),
        b"clean\n"
    );
    let stashes = cli_text(p, &["stash", "list"]);
    assert!(stashes.contains("stash@{0}"), "{stashes}");
    let saved = cli_text(p, &["stash", "show", "-p", "stash@{0}"]);
    assert!(saved.contains("+uncommitted work"), "{saved}");
}
