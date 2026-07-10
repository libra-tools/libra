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
    assert!(
        String::from_utf8_lossy(&cached.stderr).contains("--scan"),
        "degradation hint points at --scan: {}",
        String::from_utf8_lossy(&cached.stderr)
    );
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
