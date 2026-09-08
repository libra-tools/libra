//! Navigation must refuse destructive gitlink transitions before publishing HEAD.

use super::*;

mod creation;
mod tracking;

const GITLINK: &str = "0123456789abcdef0123456789abcdef01234567";

fn successful_stdout(repo: &std::path::Path, args: &[&str]) -> String {
    let output = run_libra_command(args, repo);
    assert_cli_success(&output, "gitlink navigation fixture command");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn dropping_gitlink_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    successful_stdout(
        path,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{GITLINK},vendor"),
        ],
    );
    successful_stdout(path, &["commit", "-m", "add gitlink", "--no-verify"]);
    let base = successful_stdout(path, &["rev-parse", "HEAD"]);
    successful_stdout(path, &["update-index", "--remove", "vendor"]);
    let tree = successful_stdout(path, &["write-tree"]);
    let target = successful_stdout(
        path,
        &["commit-tree", &tree, "-p", &base, "-m", "drop gitlink"],
    );
    successful_stdout(path, &["update-ref", "refs/heads/dropped", &target]);
    successful_stdout(
        path,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{GITLINK},vendor"),
        ],
    );
    std::fs::create_dir(path.join("vendor")).unwrap();
    repo
}

#[test]
fn navigation_preserves_repository_when_dropping_a_nonempty_gitlink_is_refused() {
    for command in ["switch", "checkout"] {
        for detached in [false, true] {
            // Given: a clean gitlink index with user-owned nested content.
            let repo = dropping_gitlink_repo();
            let path = repo.path();
            std::fs::write(path.join("vendor/inner.txt"), "nested user bytes\n").unwrap();
            let head = successful_stdout(path, &["rev-parse", "HEAD"]);
            let target = successful_stdout(path, &["rev-parse", "dropped"]);
            let refs = successful_stdout(path, &["show-ref"]);
            let reflog = successful_stdout(path, &["reflog", "show", "HEAD"]);
            let index = std::fs::read(path.join(".libra/index")).unwrap();
            let tracked = std::fs::read(path.join("tracked.txt")).unwrap();
            let args = if detached {
                vec![command, "--detach", target.as_str()]
            } else {
                vec![command, "dropped"]
            };

            // When: navigation attempts to drop the materialized gitlink.
            let output = run_libra_command(&args, path);

            // Then: refusal preserves HEAD, refs/reflog, index and user bytes.
            assert_eq!(output.status.code(), Some(128), "{args:?}");
            let (stderr, report) = parse_cli_error_stderr(&output.stderr);
            assert_eq!(report.error_code, "LBR-CONFLICT-002");
            assert!(stderr.contains("refusing to replace non-empty worktree directory 'vendor'"));
            assert!(stderr.contains("move or remove nested files"));
            assert_eq!(
                successful_stdout(path, &["rev-parse", "HEAD"]),
                head,
                "{args:?}"
            );
            assert_eq!(
                successful_stdout(path, &["symbolic-ref", "HEAD"]),
                "refs/heads/main"
            );
            assert_eq!(successful_stdout(path, &["show-ref"]), refs);
            assert_eq!(successful_stdout(path, &["reflog", "show", "HEAD"]), reflog);
            assert_eq!(std::fs::read(path.join(".libra/index")).unwrap(), index);
            assert_eq!(std::fs::read(path.join("tracked.txt")).unwrap(), tracked);
            assert_eq!(
                std::fs::read(path.join("vendor/inner.txt")).unwrap(),
                b"nested user bytes\n"
            );
        }
    }
}

#[test]
fn navigation_drops_an_empty_gitlink_placeholder_successfully() {
    for command in ["switch", "checkout"] {
        for detached in [false, true] {
            // Given: the gitlink has only Libra's empty directory placeholder.
            let repo = dropping_gitlink_repo();
            let path = repo.path();
            let target = successful_stdout(path, &["rev-parse", "dropped"]);
            let main = successful_stdout(path, &["rev-parse", "main"]);
            let tracked = std::fs::read(path.join("tracked.txt")).unwrap();
            let args = if detached {
                vec![command, "--detach", target.as_str()]
            } else {
                vec![command, "dropped"]
            };

            // When: navigation drops the unmaterialized gitlink.
            let output = run_libra_command(&args, path);

            // Then: HEAD/index/worktree all describe the target tree.
            assert_cli_success(&output, "empty gitlink placeholder navigation");
            assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), target);
            assert_eq!(successful_stdout(path, &["rev-parse", "main"]), main);
            assert!(!path.join("vendor").exists());
            assert_eq!(std::fs::read(path.join("tracked.txt")).unwrap(), tracked);
            assert!(successful_stdout(path, &["ls-files", "--stage", "vendor"]).is_empty());
            assert!(successful_stdout(path, &["status", "--porcelain"]).is_empty());
            let symbolic = run_libra_command(&["symbolic-ref", "HEAD"], path);
            if detached {
                assert!(!symbolic.status.success());
            } else {
                assert_cli_success(&symbolic, "branch navigation remains attached");
                assert_eq!(
                    String::from_utf8_lossy(&symbolic.stdout).trim(),
                    "refs/heads/dropped"
                );
            }
        }
    }
}
