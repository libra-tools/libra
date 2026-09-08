//! Bisect navigation must preserve repository/session state on gitlink refusal.

use std::{fs, path::Path};

use serde_json::Value;

use super::super::{
    assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command,
};

mod steps;

const GITLINK: &str = "0123456789abcdef0123456789abcdef01234567";

fn successful_stdout(path: &Path, args: &[&str]) -> String {
    let output = run_libra_command(args, path);
    assert_cli_success(&output, "bisect gitlink fixture command");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn gitlink_history() -> (tempfile::TempDir, Vec<String>) {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    let mut commits = vec![successful_stdout(path, &["rev-parse", "HEAD"])];
    for (step, pointer) in [false, false, true, true, false, true]
        .into_iter()
        .enumerate()
    {
        if pointer {
            successful_stdout(
                path,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("160000,{GITLINK},vendor"),
                ],
            );
        } else {
            successful_stdout(path, &["update-index", "--remove", "vendor"]);
        }
        let tree = successful_stdout(path, &["write-tree"]);
        let commit = successful_stdout(
            path,
            &[
                "commit-tree",
                &tree,
                "-p",
                commits.last().unwrap(),
                "-m",
                &format!("step {step}"),
            ],
        );
        commits.push(commit);
    }
    successful_stdout(path, &["update-ref", "refs/heads/main", &commits[6]]);
    fs::create_dir(path.join("vendor")).unwrap();
    (repo, commits)
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    head: String,
    symbolic: Option<String>,
    refs: String,
    reflog: String,
    index: Vec<u8>,
    tracked: Vec<u8>,
    nested: Vec<u8>,
    session: Value,
}

fn snapshot(path: &Path) -> Snapshot {
    let symbolic = run_libra_command(&["symbolic-ref", "HEAD"], path);
    let state = run_libra_command(&["--json", "bisect", "log"], path);
    assert_cli_success(&state, "read active bisect state");
    Snapshot {
        head: successful_stdout(path, &["rev-parse", "HEAD"]),
        symbolic: symbolic.status.success().then(|| {
            String::from_utf8(symbolic.stdout)
                .unwrap()
                .trim()
                .to_owned()
        }),
        refs: successful_stdout(path, &["show-ref"]),
        reflog: successful_stdout(path, &["reflog", "show", "HEAD"]),
        index: fs::read(path.join(".libra/index")).unwrap(),
        tracked: fs::read(path.join("tracked.txt")).unwrap(),
        nested: fs::read(path.join("vendor/inner.txt")).unwrap(),
        session: parse_json_stdout(&state)["data"].clone(),
    }
}

fn assert_gitlink_refusal(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(128));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(stderr.contains("refusing to replace non-empty worktree directory 'vendor'"));
    assert!(stderr.contains("move or remove nested files"));
}

#[test]
fn bisect_start_with_nonempty_gitlink_keeps_existing_cleanliness_refusal() {
    for converged in [false, true] {
        // Given: a materialized gitlink has untracked nested user content.
        let (repo, commits) = gitlink_history();
        let path = repo.path();
        fs::write(path.join("vendor/inner.txt"), "nested user bytes\n").unwrap();
        let head = successful_stdout(path, &["rev-parse", "HEAD"]);
        let index = fs::read(path.join(".libra/index")).unwrap();
        let (bad, good) = if converged { (2, 1) } else { (4, 0) };

        // When: starting a search with a nonempty submodule directory.
        let output = run_libra_command(
            &["bisect", "start", &commits[bad], "--good", &commits[good]],
            path,
        );

        // Then: the existing IncludeIgnored cleanliness gate creates no session.
        assert_eq!(output.status.code(), Some(128));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("working tree contains uncommitted changes")
        );
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            successful_stdout(path, &["symbolic-ref", "HEAD"]),
            "refs/heads/main"
        );
        assert_eq!(fs::read(path.join(".libra/index")).unwrap(), index);
        assert_eq!(
            fs::read(path.join("vendor/inner.txt")).unwrap(),
            b"nested user bytes\n"
        );
        assert!(
            !run_libra_command(&["bisect", "view"], path)
                .status
                .success()
        );
    }
}

#[test]
fn bisect_start_refuses_empty_child_before_claiming_session_or_moving_head() {
    for converged in [false, true] {
        // Given: directory-only contents pass the file-based cleanliness gate.
        let (repo, commits) = gitlink_history();
        let path = repo.path();
        let child = path.join("vendor/empty-child");
        fs::create_dir(&child).unwrap();
        let head = successful_stdout(path, &["rev-parse", "HEAD"]);
        let refs = successful_stdout(path, &["show-ref"]);
        let reflog = successful_stdout(path, &["reflog", "show", "HEAD"]);
        let index = fs::read(path.join(".libra/index")).unwrap();
        let tracked = fs::read(path.join("tracked.txt")).unwrap();
        let (bad, good) = if converged { (2, 1) } else { (4, 0) };

        // When: an initial candidate or culprit would remove that directory.
        let output = run_libra_command(
            &["bisect", "start", &commits[bad], "--good", &commits[good]],
            path,
        );

        // Then: restore preflight rejects before HEAD or session publication.
        assert_gitlink_refusal(&output);
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            successful_stdout(path, &["symbolic-ref", "HEAD"]),
            "refs/heads/main"
        );
        assert_eq!(successful_stdout(path, &["show-ref"]), refs);
        assert_eq!(successful_stdout(path, &["reflog", "show", "HEAD"]), reflog);
        assert_eq!(fs::read(path.join(".libra/index")).unwrap(), index);
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), tracked);
        assert!(child.is_dir());
        assert_eq!(fs::read_dir(&child).unwrap().count(), 0);
        assert_eq!(fs::read_dir(path.join("vendor")).unwrap().count(), 1);
        let view = run_libra_command(&["bisect", "view"], path);
        assert!(
            !view.status.success(),
            "refused start must not claim a session"
        );
        let (stderr, _) = parse_cli_error_stderr(&view.stderr);
        assert!(stderr.contains("not in an active bisect"));
    }
}

#[test]
fn bisect_start_drops_empty_gitlink_placeholder_at_next_or_converged_target() {
    for converged in [false, true] {
        // Given: only Libra's empty placeholder exists at the gitlink path.
        let (repo, commits) = gitlink_history();
        let path = repo.path();
        let (bad, good) = if converged { (2, 1) } else { (4, 0) };

        // When: the initial candidate or culprit drops the gitlink.
        let output = run_libra_command(
            &["bisect", "start", &commits[bad], "--good", &commits[good]],
            path,
        );

        // Then: candidate HEAD, session, index and worktree agree.
        assert_cli_success(&output, "bisect start removes empty placeholder");
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), commits[2]);
        assert!(!path.join("vendor").exists());
        assert!(successful_stdout(path, &["ls-files", "--stage", "vendor"]).is_empty());
        let view = run_libra_command(&["--json", "bisect", "view"], path);
        assert_cli_success(&view, "read started bisect");
        let data = &parse_json_stdout(&view)["data"];
        assert_eq!(data["current"], commits[2]);
        assert_eq!(data["completed"], converged);
        successful_stdout(path, &["bisect", "reset"]);
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), commits[6]);
    }
}
