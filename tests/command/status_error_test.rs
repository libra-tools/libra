//! Error code validation tests for `libra status`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use libra::internal::{db::get_db_conn_instance, model::reference};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serial_test::serial;
use tempfile::tempdir;

use super::{
    ChangeDirGuard, assert_cli_success, configure_identity_via_cli, create_committed_repo_via_cli,
    init_repo_via_cli, parse_cli_error_stderr, run_libra_command,
};

// ---------------------------------------------------------------------------
// Outside repository → LBR-REPO-001
// ---------------------------------------------------------------------------

#[test]
fn status_outside_repo_returns_repo_not_found() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["status"], temp.path());
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a libra repository"));
}

// ---------------------------------------------------------------------------
// Corrupt index → LBR-REPO-002
// ---------------------------------------------------------------------------

#[test]
fn status_corrupt_index_returns_repo_corrupt() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    // Corrupt the index file
    fs::write(repo.path().join(".libra").join("index"), b"corrupted").unwrap();

    let output = run_libra_command(&["status"], repo.path());
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal:"),
        "stderr should contain fatal prefix: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --json mode: errors on stderr, stdout clean
// ---------------------------------------------------------------------------

#[test]
fn status_json_error_keeps_stdout_clean() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["--json", "status"], temp.path());
    assert!(!output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    // stdout should be clean (no JSON output on success path)
    assert!(
        stdout.trim().is_empty(),
        "stdout should be clean on error: {stdout}"
    );

    // stderr should contain the error
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.trim().is_empty(), "stderr should have error info");
}

#[tokio::test]
#[serial(cwd)]
async fn status_corrupt_head_reference_returns_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let db = get_db_conn_instance().await;
    let head = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
        .filter(reference::Column::Remote.is_null())
        .one(&db)
        .await
        .unwrap()
        .expect("expected HEAD row");
    let mut head: reference::ActiveModel = head.into();
    head.name = Set(None);
    head.commit = Set(Some("not-a-valid-hash".to_string()));
    head.update(&db).await.unwrap();

    let output = run_libra_command(&["status"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("invalid detached HEAD commit hash"),
        "unexpected stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Missing nested HEAD tree → LBR-REPO-002, never a panic or partial status
// ---------------------------------------------------------------------------

fn repo_with_nested_head_tree() -> (tempfile::TempDir, String, std::path::PathBuf) {
    let repo = create_committed_repo_via_cli();
    fs::create_dir_all(repo.path().join("outer/inner")).unwrap();
    fs::write(repo.path().join("outer/inner/file.txt"), "nested content\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "outer"], repo.path()),
        "stage nested fixture",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "nested tree", "--no-verify"], repo.path()),
        "commit nested fixture",
    );
    let listing = run_libra_command(&["ls-tree", "HEAD:outer"], repo.path());
    assert_cli_success(&listing, "list nested HEAD tree");
    let listing = String::from_utf8(listing.stdout).unwrap();
    let oid = listing
        .lines()
        .find(|line| line.ends_with("\tinner"))
        .and_then(|line| line.split_whitespace().nth(2))
        .expect("inner tree object ID")
        .to_string();
    let object = repo
        .path()
        .join(".libra/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    assert!(object.is_file(), "fixture must contain a loose nested tree");
    (repo, oid, object)
}

fn assert_nested_tree_error(repo: &std::path::Path, oid: &str) {
    let index = fs::read(repo.join(".libra/index")).unwrap();
    for args in [
        vec!["status"],
        vec!["status", "--no-renames"],
        vec!["status", "--porcelain=v1"],
        vec!["status", "--porcelain=v2"],
        vec!["--json", "status"],
        vec!["--machine", "status"],
    ] {
        let output = run_libra_command(&args, repo);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(128), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}: partial status output");
        let (_, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-REPO-002", "{args:?}");
        assert_eq!(report.exit_code, 128, "{args:?}");
        assert!(report.message.contains(oid), "{args:?}: {report:?}");
        assert!(
            report.hints.iter().any(|hint| hint.contains("libra fsck")),
            "{report:?}"
        );
        if args[0] == "--json" || args[0] == "--machine" {
            serde_json::from_slice::<serde_json::Value>(&output.stderr)
                .expect("structured modes must emit exactly one JSON error");
        }
        assert_eq!(
            fs::read(repo.join(".libra/index")).unwrap(),
            index,
            "{args:?}: index changed"
        );
    }
}

#[test]
fn status_nested_tree_missing_returns_repo_error() {
    let (repo, oid, object) = repo_with_nested_head_tree();
    fs::remove_file(object).unwrap();
    assert_nested_tree_error(repo.path(), &oid);
}

#[test]
fn status_nested_tree_corrupt_object_returns_repo_error() {
    let (repo, oid, object) = repo_with_nested_head_tree();
    fs::write(object, b"not a zlib object").unwrap();
    assert_nested_tree_error(repo.path(), &oid);
}

#[test]
fn status_nested_tree_invalid_entries_returns_repo_error() {
    use std::io::Write;

    use flate2::{Compression, write::ZlibEncoder};

    let (repo, oid, object) = repo_with_nested_head_tree();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"tree 3\0bad").unwrap();
    fs::write(object, encoder.finish().unwrap()).unwrap();
    assert_nested_tree_error(repo.path(), &oid);
}

#[test]
fn status_nested_tree_gitlink_does_not_require_target_object() {
    let (repo, _, _) = repo_with_nested_head_tree();
    let phantom = "1111111111111111111111111111111111111111";
    let spec = format!("160000,{phantom},outer/inner/module");
    assert_cli_success(
        &run_libra_command(
            &["update-index", "--add", "--cacheinfo", &spec],
            repo.path(),
        ),
        "stage nested gitlink with absent commit",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "nested gitlink", "--no-verify"],
            repo.path(),
        ),
        "commit nested gitlink",
    );
    for args in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["status", "--porcelain=v2"],
    ] {
        assert_cli_success(
            &run_libra_command(&args, repo.path()),
            "status with nested gitlink",
        );
    }
}

// ---------------------------------------------------------------------------
// --exit-code: dirty → exit 1
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_dirty_returns_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    // Modify tracked file → dirty
    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["status", "--exit-code"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "dirty repo with --exit-code should exit 1"
    );
}

// ---------------------------------------------------------------------------
// --exit-code: clean → exit 0
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_clean_returns_0() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    let output = run_libra_command(&["status", "--exit-code"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "clean repo with --exit-code should exit 0"
    );
}

// ---------------------------------------------------------------------------
// --exit-code --quiet: dirty → exit 1, no output
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_quiet_dirty_silent_exit_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--quiet", "status", "--exit-code"], repo.path());
    assert_eq!(output.status.code(), Some(1));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().is_empty(), "quiet should suppress stdout");
}

// ---------------------------------------------------------------------------
// --quiet without --exit-code: always exit 0
// ---------------------------------------------------------------------------

#[test]
fn status_quiet_without_exit_code_always_0() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--quiet", "status"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet without --exit-code should always exit 0"
    );
}

// ---------------------------------------------------------------------------
// --exit-code respects --untracked-files=no filter
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_untracked_only_with_filter_is_clean() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    // Only untracked file, no tracked modifications
    fs::write(repo.path().join("untracked.txt"), "new").unwrap();

    let output = run_libra_command(
        &["status", "--exit-code", "--untracked-files=no"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "only untracked files with --untracked-files=no should be clean"
    );
}

// ---------------------------------------------------------------------------
// --exit-code --json: JSON output + exit 1
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_json_dirty_returns_json_and_exit_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--json", "status", "--exit-code"], repo.path());
    assert_eq!(output.status.code(), Some(1));

    // JSON should still be valid on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got: {stdout}\nerror: {e}"));
    assert_eq!(
        parsed["ok"], true,
        "JSON ok should be true even with exit 1"
    );
    assert_eq!(parsed["data"]["is_clean"], false);
}

#[test]
fn i1062_status_invalid_attribute_source_never_reports_clean() {
    use super::{
        add_test::{i1062_ok, i1062_run},
        status_test::i1062_committed_scripts,
    };
    let temp = i1062_committed_scripts("sha1");
    let repo = temp.path().join("repo");
    i1062_ok(&repo, &["add", "probe.ps1", "probe.bat", "probe.cmd"]);
    i1062_ok(&repo, &["--json", "status", "--scan"]);
    fs::create_dir_all(repo.join(".libra/info/attributes")).unwrap();
    for args in [
        vec!["--json", "status"],
        vec!["--json", "status", "--scan"],
        vec!["--json", "status", "--check-dirty"],
    ] {
        let output = i1062_run(&repo, &args);
        if output.status.success() {
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                !doc["data"]["io_blocked"].as_array().unwrap().is_empty(),
                "{args:?}: {doc}"
            );
        }
    }
    assert!(
        !i1062_run(&repo, &["status", "--porcelain"])
            .status
            .success()
    );
}

#[test]
fn i861_missing_index_evidence_is_blocked_and_never_clean() {
    use super::add_test::{i861_damage_index_blob, i1062_run};
    let temp = super::status_test::i861_committed_scripts("sha256");
    let repo = temp.path().join("repo");
    i861_damage_index_blob(&repo, "probe.ps1", false);
    let before = fs::read(repo.join(".libra/index")).unwrap();
    for mode in [None, Some("--scan"), Some("--check-dirty")] {
        let mut args = vec!["--json", "status"];
        args.extend(mode);
        let output = i1062_run(&repo, &args);
        if output.status.success() {
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                !doc["data"]["io_blocked"].as_array().unwrap().is_empty(),
                "{doc}"
            );
            assert_ne!(doc["data"]["is_clean"], true, "{doc}");
            assert!(
                doc["data"]["unstaged"]["deleted"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{doc}"
            );
        }
    }
    assert!(
        !i1062_run(&repo, &["status", "--porcelain"])
            .status
            .success()
    );
    assert_eq!(fs::read(repo.join(".libra/index")).unwrap(), before);
}
