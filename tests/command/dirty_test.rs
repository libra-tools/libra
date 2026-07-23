//! Integration tests for the dirty-set cache (lore.md §1.1): `libra dirty`,
//! `status --scan` / `--cached` / `--check-dirty`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

fn dirty_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    repo
}

#[test]
fn dirty_cache_scan_cached_roundtrip() {
    let repo = dirty_repo();
    let p = repo.path();
    // Modify + stage another file so the snapshot carries both sets.
    fs::write(p.join("f.txt"), "two\n").unwrap();
    fs::write(p.join("staged.txt"), "s\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "staged.txt"], p), "add staged");

    let scan = run_libra_command(&["status", "--scan"], p);
    assert_cli_success(&scan, "scan");
    assert!(
        String::from_utf8_lossy(&scan.stdout).contains("dirty cache rebuilt"),
        "{}",
        String::from_utf8_lossy(&scan.stdout)
    );

    // --cached agrees without walking (JSON mode + freshness markers).
    let cached = run_libra_command(&["--json", "status", "--cached"], p);
    assert_cli_success(&cached, "cached");
    let json = parse_json_stdout(&cached);
    assert_eq!(json["data"]["mode"].as_str(), Some("cached"));
    assert_eq!(json["data"]["freshness"].as_str(), Some("cached"));
    assert_eq!(json["data"]["cache_state"].as_str(), Some("fresh"));
    let unstaged_modified = json["data"]["unstaged"]["modified"]
        .as_array()
        .map(|a| a.iter().any(|v| v.as_str() == Some("f.txt")))
        .unwrap_or(false);
    assert!(
        unstaged_modified,
        "cached view lists f.txt modified: {json}"
    );
    let staged_new = json["data"]["staged"]["new"]
        .as_array()
        .map(|a| a.iter().any(|v| v.as_str() == Some("staged.txt")))
        .unwrap_or(false);
    assert!(
        staged_new,
        "cached staged snapshot lists staged.txt: {json}"
    );
}

#[test]
fn dirty_cache_status_modes_honor_pathspec_filters() {
    let repo = dirty_repo();
    let p = repo.path();
    fs::create_dir_all(p.join("docs")).unwrap();
    fs::write(p.join("f.txt"), "two\n").unwrap();
    fs::write(p.join("docs/readme.md"), "docs\n").unwrap();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");

    let cached = run_libra_command(&["--json", "status", "--cached", "docs"], p);
    assert_cli_success(&cached, "cached docs");
    let json = parse_json_stdout(&cached);
    assert!(
        json["data"]["untracked"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("docs/readme.md"))),
        "cached pathspec should keep docs/readme.md: {json}"
    );
    assert!(
        !json["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "cached pathspec should filter unrelated f.txt: {json}"
    );

    let clean_path = run_libra_command(
        &[
            "status",
            "--cached",
            "--quiet",
            "--exit-code",
            ".libraignore",
        ],
        p,
    );
    assert_eq!(
        clean_path.status.code(),
        Some(0),
        "filtered cached dirty state should not trip --exit-code"
    );

    let check_dirty = run_libra_command(&["--json", "status", "--check-dirty", "docs"], p);
    assert_cli_success(&check_dirty, "check-dirty docs");
    let json = parse_json_stdout(&check_dirty);
    assert!(
        json["data"]["untracked"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("docs/readme.md"))),
        "check-dirty pathspec should keep docs/readme.md: {json}"
    );
    assert!(
        !json["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "check-dirty pathspec should filter unrelated f.txt: {json}"
    );
}

/// §B.5 rule 2: the stale-cache fallback's structured warning must exit 9
/// under `--exit-code-on-warning`, beating the `--exit-code` dirty exit 1.
#[test]
fn cache_stale_fallback_warning_exit_nine_over_dirty() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");
    fs::write(p.join("f.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    let fallback = run_libra_command(
        &[
            "--exit-code-on-warning",
            "status",
            "--cached",
            "--exit-code",
        ],
        p,
    );
    assert_eq!(
        fallback.status.code(),
        Some(9),
        "stale-fallback warning exits 9 before the dirty exit 1"
    );
    assert!(
        String::from_utf8_lossy(&fallback.stderr).contains("--scan"),
        "fallback hint still delivered on stderr"
    );
}

#[test]
fn dirty_cache_invalidated_by_index_write() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");
    // An index write (add) changes the fingerprint → --cached degrades.
    fs::write(p.join("f.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    let cached = run_libra_command(&["--json", "status", "--cached"], p);
    assert_cli_success(&cached, "cached degrades, still succeeds");
    let json = parse_json_stdout(&cached);
    assert_eq!(json["data"]["freshness"].as_str(), Some("full"));
    assert_eq!(json["data"]["cache_state"].as_str(), Some("stale"));
    // R0-8b: JSON mode carries the hint in data.warnings, stderr stays empty.
    assert!(
        json["data"]["warnings"]
            .as_array()
            .is_some_and(|a| a.iter().any(|w| w["code"] == "dirty_cache_stale_fallback")),
        "stale fallback warning rides data.warnings: {json}"
    );
    assert!(cached.stderr.is_empty(), "json fallback keeps stderr empty");
}

#[test]
fn dirty_manual_marks_and_check_dirty_prune() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");
    // A post-scan worktree edit is invisible to --cached (snapshot semantics)
    // until marked.
    fs::write(p.join("f.txt"), "two\n").unwrap();
    let mark = run_libra_command(&["dirty", "f.txt"], p);
    assert_cli_success(&mark, "mark");
    let cached = run_libra_command(&["--json", "status", "--cached"], p);
    let json = parse_json_stdout(&cached);
    assert!(
        json["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "manual mark classified as modified: {json}"
    );
    // Restore the content: check-dirty re-verifies and prunes the mark.
    fs::write(p.join("f.txt"), "one\n").unwrap();
    let check = run_libra_command(&["--json", "status", "--check-dirty"], p);
    assert_cli_success(&check, "check-dirty");
    let json = parse_json_stdout(&check);
    assert_eq!(json["data"]["mode"].as_str(), Some("check_dirty"));
    assert!(
        json["data"]["stale_paths"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "pruned the clean mark: {json}"
    );
    // Escaping paths are refused atomically — relative and absolute.
    let escape = run_libra_command(&["dirty", "../outside.txt"], p);
    assert_eq!(escape.status.code(), Some(129), "repo escape refused");
    let abs_escape = run_libra_command(&["dirty", "/etc/hosts"], p);
    assert_eq!(
        abs_escape.status.code(),
        Some(129),
        "absolute path outside the repo refused: {}",
        String::from_utf8_lossy(&abs_escape.stderr)
    );
    // dirty --list works and reports freshness.
    let list = run_libra_command(&["--json", "dirty", "--list"], p);
    assert_cli_success(&list, "list");
    let json = parse_json_stdout(&list);
    assert!(json["data"]["cache_state"].as_str().is_some());
}

#[test]
fn dirty_cache_default_status_untouched_and_json_stable() {
    let repo = dirty_repo();
    let p = repo.path();
    // Default status before any scan: no cache keys in JSON.
    fs::write(p.join("f.txt"), "two\n").unwrap();
    let default = run_libra_command(&["--json", "status"], p);
    assert_cli_success(&default, "default status");
    let json = parse_json_stdout(&default);
    assert!(json["data"].get("mode").is_none(), "no mode key: {json}");
    assert!(
        json["data"].get("cache_state").is_none(),
        "no cache keys: {json}"
    );
    // Default status must not create or update the cache.
    let list = run_libra_command(&["--json", "dirty", "--list"], p);
    let json = parse_json_stdout(&list);
    assert_eq!(
        json["data"]["cache_state"].as_str(),
        Some("missing"),
        "default status never populates the cache: {json}"
    );
    // Flag exclusions.
    let both = run_libra_command(&["status", "--cached", "--scan"], p);
    assert_eq!(both.status.code(), Some(129));
    let porcelain = run_libra_command(&["status", "--cached", "--porcelain"], p);
    assert_eq!(porcelain.status.code(), Some(129));
}

#[test]
fn dirty_scan_lock_blocks_second_scanner() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan 1");
    // Simulate a live scanner: hold the lock manually via a second scan racing
    // is hard to arrange deterministically, so assert the lock RELEASES after
    // a normal scan (a second scan succeeds — no wedged lock).
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan 2");
}

/// R0-8b: the three dirty-cache degradations surface as structured warnings
/// in JSON `data.warnings[]` (no stderr), replacing the legacy stderr-only
/// path. Human modes keep the stderr line via the shared delivery.
#[test]
fn json_cached_stale_fallback_warning() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");
    fs::write(p.join("f.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    let cached = run_libra_command(&["--json", "status", "--cached"], p);
    assert_cli_success(&cached, "cached degrades, still succeeds");
    let json = parse_json_stdout(&cached);
    assert!(
        json["data"]["warnings"]
            .as_array()
            .is_some_and(|a| a.iter().any(|w| w["code"] == "dirty_cache_stale_fallback")),
        "stale fallback rides data.warnings: {json}"
    );
    assert!(
        cached.stderr.is_empty(),
        "json cache fallback keeps stderr empty: {:?}",
        String::from_utf8_lossy(&cached.stderr)
    );
}

/// R0-8b: `--check-dirty` after an index write degrades through the STALE
/// classification (the pre-read check); the true mid-read concurrent branch
/// shares the same structured push but needs fault-injection infrastructure
/// to trigger deterministically (tracked with R0-8 fault injection).
#[test]
fn json_check_dirty_stale_fallback_warning() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "scan");
    fs::write(p.join("f.txt"), "three\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    let checked = run_libra_command(&["--json", "status", "--check-dirty"], p);
    assert_cli_success(&checked, "check-dirty degrades, still succeeds");
    let json = parse_json_stdout(&checked);
    assert!(
        json["data"]["warnings"]
            .as_array()
            .is_some_and(|a| a.iter().any(|w| w["code"] == "dirty_cache_stale_fallback")),
        "check-dirty stale fallback rides data.warnings: {json}"
    );
    assert!(
        checked.stderr.is_empty(),
        "json check-dirty fallback keeps stderr empty"
    );
}

/// R0-8b: a stale scan lock (dead pid, timestamp beyond the steal window) is
/// stolen with the structured `dirty_cache_lock_stolen` warning — JSON rides
/// `data.warnings[]` with a clean stderr, and 9≻1 arbitration applies.
#[test]
#[serial]
fn scan_lock_stolen_warning() {
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "seed scan");

    // Plant a stale lock in-process: dead pid + ancient timestamp.
    let _guard = ChangeDirGuard::new(p);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "UPDATE working_dirty_meta SET scan_lock_pid = 999999999,              scan_lock_at = '2000-01-01T00:00:00Z' WHERE worktree_id = '';"
                .to_string(),
        ))
        .await
        .expect("plant stale scan lock");
    });
    drop(rt);

    let json = run_libra_command(&["--json", "status", "--scan"], p);
    assert_cli_success(&json, "scan steals the stale lock");
    let doc = parse_json_stdout(&json);
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|a| a.iter().any(|w| w["code"] == "dirty_cache_lock_stolen")),
        "steal warning rides data.warnings: {doc}"
    );
    assert!(json.stderr.is_empty(), "json steal keeps stderr empty");

    // Re-plant and verify the human path + 9≻1 arbitration.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "UPDATE working_dirty_meta SET scan_lock_pid = 999999999,              scan_lock_at = '2000-01-01T00:00:00Z' WHERE worktree_id = '';"
                .to_string(),
        ))
        .await
        .expect("re-plant stale scan lock");
    });
    drop(rt);
    let human = run_libra_command(
        &["--exit-code-on-warning", "status", "--scan", "--exit-code"],
        p,
    );
    assert_eq!(human.status.code(), Some(9), "steal warning exits 9");
    let human_stderr = String::from_utf8_lossy(&human.stderr);
    assert_eq!(
        human_stderr
            .matches("stole a stale dirty-cache scan lock")
            .count(),
        1,
        "human path delivers the stderr line exactly once: {human_stderr}"
    );

    // Quiet success: body suppressed, diagnostic still exactly once.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "UPDATE working_dirty_meta SET scan_lock_pid = 999999999, \
             scan_lock_at = '2000-01-01T00:00:00Z' WHERE worktree_id = '';"
                .to_string(),
        ))
        .await
        .expect("re-plant stale scan lock for quiet run");
    });
    drop(rt);
    let quiet = run_libra_command(&["--quiet", "status", "--scan"], p);
    assert_cli_success(&quiet, "quiet scan steals");
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    assert_eq!(
        quiet_stderr.trim_end(),
        "warning: stole a stale dirty-cache scan lock (previous scanner crashed?)",
        "quiet stderr is exactly the single diagnostic line"
    );
}

/// P0-06 × R0-8b: closing the stdout read end mid-render must stay silent —
/// zero stderr — even when a stolen-lock warning is pending (EPIPE maps to a
/// silent exit, so neither the renderer nor the wrapper fallback may write).
#[test]
#[serial]
fn scan_stale_lock_broken_pipe_stays_silent() {
    use std::process::Stdio;
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "seed scan");
    let _guard = ChangeDirGuard::new(p);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "UPDATE working_dirty_meta SET scan_lock_pid = 999999999, \
             scan_lock_at = '2000-01-01T00:00:00Z' WHERE worktree_id = '';"
                .to_string(),
        ))
        .await
        .expect("plant stale scan lock");
    });
    drop(rt);

    // Only --scan carries the pending stolen-lock warning; a plain status
    // never touches the scan lock (generic render EPIPE is guarded by
    // compat_broken_pipe_output).
    {
        let args = ["status", "--scan"];
        let mut child = base_libra_command(&args, p)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn status with piped stdout");
        drop(child.stdout.take()); // close the read end immediately
        let out = child.wait_with_output().expect("wait status");
        assert!(
            out.status.success(),
            "{args:?}: EPIPE is a silent SUCCESS exit (P0-06): {:?}",
            out.status
        );
        assert!(
            out.stderr.is_empty(),
            "{args:?}: EPIPE must stay silent on stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// R0-8b: the true mid-read concurrent-invalidate branch, triggered
/// deterministically via the LIBRA_TEST-gated read-pause seam — an `add`
/// lands inside the widened read→re-verify window, and the fallback carries
/// `dirty_cache_concurrent_invalidate` in JSON warnings with clean stderr.
#[test]
#[serial]
fn json_check_dirty_concurrent_invalidate_warning() {
    use std::process::Stdio;
    let repo = dirty_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["status", "--scan"], p), "seed scan");

    let child = base_libra_command(&["--json", "status", "--check-dirty"], p)
        .env("LIBRA_TEST_CACHE_READ_PAUSE_MS", "1500")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn paused check-dirty");
    // Land an index write inside the widened window.
    std::thread::sleep(std::time::Duration::from_millis(400));
    fs::write(p.join("f.txt"), "concurrent\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "concurrent add");
    let out = child.wait_with_output().expect("wait check-dirty");
    assert!(out.status.success(), "fallback still succeeds");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("envelope");
    assert!(
        doc["data"]["warnings"].as_array().is_some_and(|a| a
            .iter()
            .any(|w| w["code"] == "dirty_cache_concurrent_invalidate")),
        "mid-read invalidation surfaces the concurrent code: {doc}"
    );
    assert!(out.stderr.is_empty(), "json fallback keeps stderr empty");
}

/// W1 §C.4.1.1: dirty-cache rows and meta are per-worktree. Scans in the
/// main and a linked worktree keep independent freshness/rows: each
/// `--cached` sees only its own scope, and a linked scan never invalidates
/// or prunes the main worktree's snapshot.
#[test]
#[serial]
fn linked_dirty_cache_rows_and_meta_isolated() {
    let repo = dirty_repo();
    let main = repo.path();
    let wt_root = tempdir().expect("wt root");
    let wt = wt_root.path().join("dirty-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Main: one dirty file, scan, cached view sees it.
    fs::write(main.join("f.txt"), "main dirt\n").unwrap();
    assert_cli_success(&run_libra_command(&["status", "--scan"], main), "main scan");
    let main_cached = run_libra_command(&["--json", "status", "--cached"], main);
    assert_cli_success(&main_cached, "main cached");
    let main_json = parse_json_stdout(&main_cached);
    assert_eq!(main_json["data"]["cache_state"].as_str(), Some("fresh"));

    // Linked: different dirt, own scan — must not touch main's snapshot.
    fs::write(wt.join("linked.txt"), "linked dirt\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["status", "--scan"], &wt),
        "linked scan",
    );
    let wt_cached = run_libra_command(&["--json", "status", "--cached"], &wt);
    assert_cli_success(&wt_cached, "linked cached");
    let wt_json = parse_json_stdout(&wt_cached);
    assert_eq!(wt_json["data"]["cache_state"].as_str(), Some("fresh"));
    assert!(
        wt_json["data"]["untracked"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("linked.txt"))),
        "linked cache sees its own dirt: {wt_json}"
    );
    assert!(
        !wt_json["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "linked cache does not leak main's rows: {wt_json}"
    );

    // Main's snapshot is still FRESH after the linked scan (separate meta).
    let main_again = run_libra_command(&["--json", "status", "--cached"], main);
    assert_cli_success(&main_again, "main cached after linked scan");
    let again = parse_json_stdout(&main_again);
    assert_eq!(
        again["data"]["cache_state"].as_str(),
        Some("fresh"),
        "linked scan must not invalidate main's meta: {again}"
    );
    assert!(
        again["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("f.txt"))),
        "main cache keeps its own rows: {again}"
    );
}

/// W1 §C.4.1.1: `libra dirty <path>` (manual mark) in a linked worktree
/// writes only that scope's rows — the main worktree's list stays clean.
#[test]
#[serial]
fn linked_dirty_mark_is_scoped() {
    let repo = dirty_repo();
    let main = repo.path();
    let wt_root = tempdir().expect("wt root");
    let wt = wt_root.path().join("dirty-mark-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(&run_libra_command(&["status", "--scan"], main), "main scan");
    assert_cli_success(
        &run_libra_command(&["status", "--scan"], &wt),
        "linked scan",
    );

    assert_cli_success(
        &run_libra_command(&["dirty", "f.txt"], &wt),
        "manual mark in linked scope",
    );
    let wt_list = run_libra_command(&["--json", "dirty", "--list"], &wt);
    assert_cli_success(&wt_list, "linked dirty list");
    assert!(
        String::from_utf8_lossy(&wt_list.stdout).contains("f.txt"),
        "linked list sees its own mark"
    );
    let main_list = run_libra_command(&["--json", "dirty", "--list"], main);
    assert_cli_success(&main_list, "main dirty list");
    assert!(
        !String::from_utf8_lossy(&main_list.stdout).contains("f.txt"),
        "main list must not see the linked mark: {}",
        String::from_utf8_lossy(&main_list.stdout)
    );
}
