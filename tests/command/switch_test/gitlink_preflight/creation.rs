//! Creating or resetting a branch must not publish refs on a refused restore.

use super::*;

#[test]
fn navigation_preserves_branch_refs_when_gitlink_creation_or_reset_is_refused() {
    for (command, create, reset) in [("switch", "-c", "-C"), ("checkout", "-b", "-B")] {
        for (flag, existing) in [(create, false), (reset, true)] {
            for force in [false, true] {
                // Given: materialized gitlink and optionally an existing topic ref.
                let repo = dropping_gitlink_repo();
                let path = repo.path();
                let head = successful_stdout(path, &["rev-parse", "HEAD"]);
                if existing {
                    successful_stdout(path, &["update-ref", "refs/heads/topic", &head]);
                }
                std::fs::write(path.join("vendor/inner.txt"), "nested user bytes\n").unwrap();
                if force {
                    std::fs::write(path.join("tracked.txt"), "local tracked edit\n").unwrap();
                }
                let tracked = std::fs::read(path.join("tracked.txt")).unwrap();
                let refs = successful_stdout(path, &["show-ref"]);
                let reflog = successful_stdout(path, &["reflog", "show", "HEAD"]);
                let index = std::fs::read(path.join(".libra/index")).unwrap();
                let mut args = vec![command, flag, "topic", "dropped"];
                if force {
                    args.push("--force");
                }

                // When: creating/resetting topic would drop the gitlink.
                let output = run_libra_command(&args, path);

                // Then: neither a new ref nor a reset ref escapes the refusal.
                assert_eq!(output.status.code(), Some(128), "{args:?}");
                let (stderr, report) = parse_cli_error_stderr(&output.stderr);
                assert_eq!(report.error_code, "LBR-CONFLICT-002");
                assert!(stderr.contains("non-empty worktree directory 'vendor'"));
                assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), head);
                assert_eq!(
                    successful_stdout(path, &["symbolic-ref", "HEAD"]),
                    "refs/heads/main"
                );
                assert_eq!(successful_stdout(path, &["show-ref"]), refs, "{args:?}");
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
}

#[test]
fn navigation_keeps_nonempty_gitlink_when_the_target_retains_its_pointer() {
    for command in ["switch", "checkout"] {
        // Given: the target retains the same gitlink backed by user content.
        let repo = dropping_gitlink_repo();
        let path = repo.path();
        let head = successful_stdout(path, &["rev-parse", "HEAD"]);
        successful_stdout(path, &["update-ref", "refs/heads/retained", &head]);
        std::fs::write(path.join("vendor/inner.txt"), "nested user bytes\n").unwrap();

        // When: switching branches without removing or changing the pointer.
        let output = run_libra_command(&[command, "retained"], path);

        // Then: navigation succeeds without touching the submodule contents.
        assert_cli_success(&output, "retained gitlink navigation");
        assert_eq!(
            successful_stdout(path, &["symbolic-ref", "HEAD"]),
            "refs/heads/retained"
        );
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            successful_stdout(path, &["ls-files", "--stage", "vendor"]),
            format!("160000 {GITLINK} 0\tvendor")
        );
        assert_eq!(
            std::fs::read(path.join("vendor/inner.txt")).unwrap(),
            b"nested user bytes\n"
        );
    }
}
