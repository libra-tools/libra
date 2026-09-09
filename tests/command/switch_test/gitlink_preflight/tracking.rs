//! Tracking branch setup is also guarded before local refs/config are published.

use super::*;

#[test]
fn switch_tracking_preserves_refs_and_config_when_gitlink_restore_is_refused() {
    for explicit_track in [false, true] {
        // Given: a fetched local remote whose topic tree drops the gitlink.
        let remote = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "--initial-branch=topic", "--object-format=sha1"],
            vec![
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-m",
                "remote target",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(remote.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", remote.path().join("absent-config"))
                .output()
                .unwrap();
            assert_cli_success(&output, "create local Git remote");
        }
        let repo = dropping_gitlink_repo();
        let path = repo.path();
        successful_stdout(
            path,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        successful_stdout(path, &["fetch", "origin"]);
        std::fs::write(path.join("vendor/inner.txt"), "nested user bytes\n").unwrap();
        let head = successful_stdout(path, &["rev-parse", "HEAD"]);
        let refs = successful_stdout(path, &["show-ref"]);
        let config = successful_stdout(path, &["config", "--local", "--list"]);
        let reflog = successful_stdout(path, &["reflog", "show", "HEAD"]);
        let index = std::fs::read(path.join(".libra/index")).unwrap();
        let args = if explicit_track {
            vec!["switch", "--track", "origin/topic"]
        } else {
            vec!["switch", "topic"]
        };

        // When: explicit or guessed tracking would remove the materialized gitlink.
        let output = run_libra_command(&args, path);

        // Then: no local topic/upstream config or navigation change is published.
        assert_eq!(output.status.code(), Some(128), "{args:?}");
        let (stderr, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-CONFLICT-002");
        assert!(stderr.contains("non-empty worktree directory 'vendor'"));
        assert_eq!(successful_stdout(path, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            successful_stdout(path, &["symbolic-ref", "HEAD"]),
            "refs/heads/main"
        );
        assert_eq!(successful_stdout(path, &["show-ref"]), refs);
        assert_eq!(
            successful_stdout(path, &["config", "--local", "--list"]),
            config
        );
        assert_eq!(successful_stdout(path, &["reflog", "show", "HEAD"]), reflog);
        assert_eq!(std::fs::read(path.join(".libra/index")).unwrap(), index);
        assert_eq!(
            std::fs::read(path.join("vendor/inner.txt")).unwrap(),
            b"nested user bytes\n"
        );
    }
}
