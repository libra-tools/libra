//! Integration tests for file case-change handling (lore.md §1.14).
//!
//! CI runs on a case-SENSITIVE filesystem, so the case-insensitive states
//! are fabricated by forcing `core.ignorecase=true`; the mv two-step /
//! same-inode paths that need a REAL case-insensitive FS are covered by the
//! same-file/fold unit tests plus cfg-gated logic, and the plain rename path
//! (which case-sensitive FSes take) is exercised here directly.
//!
//! **Layer:** L1 — deterministic.

use libra::utils::ignore::IgnorePolicy;
use serial_test::serial;

use super::*;

fn case_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("Foo.txt"), "content\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "Foo.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    repo
}

fn skip_case_twin_fixture_on_case_insensitive_host(p: &std::path::Path, scenario: &str) -> bool {
    if libra::utils::path_case::probe_dir_ignore_case(p) {
        eprintln!("skipping {scenario}: host filesystem is case-insensitive");
        return true;
    }
    false
}

#[tokio::test]
#[serial]
async fn status_sync_wrappers_honor_core_ignorecase_override() {
    let repo = case_repo();
    let p = repo.path();
    let host_ignore_case = libra::utils::path_case::probe_dir_ignore_case(p);
    let expected_alias_is_new = host_ignore_case;
    let configured_ignorecase = if host_ignore_case { "false" } else { "true" };
    assert_cli_success(
        &run_libra_command(&["config", "core.ignorecase", configured_ignorecase], p),
        "force ignorecase",
    );
    if host_ignore_case {
        fs::rename(p.join("Foo.txt"), p.join("case-tmp.txt")).unwrap();
        fs::rename(p.join("case-tmp.txt"), p.join("foo.txt")).unwrap();
    } else if let Err(error) = fs::hard_link(p.join("Foo.txt"), p.join("foo.txt")) {
        eprintln!("skipping ignorecase override status wrapper test: hard link failed: {error}");
        return;
    }
    let _guard = ChangeDirGuard::new(p);

    let unstaged = libra::command::status::changes_to_be_staged_with_policy(IgnorePolicy::Respect)
        .expect("status policy wrapper should read worktree");
    assert_eq!(
        unstaged.new.iter().any(|path| path == Path::new("foo.txt")),
        expected_alias_is_new,
        "core.ignorecase={configured_ignorecase} should control same-file case aliases in policy wrapper: {unstaged:?}"
    );

    let (safe_visible, _) = libra::command::status::changes_to_be_staged_split_safe()
        .expect("safe split wrapper should read worktree");
    assert_eq!(
        safe_visible
            .new
            .iter()
            .any(|path| path == Path::new("foo.txt")),
        expected_alias_is_new,
        "core.ignorecase={configured_ignorecase} should control same-file case aliases in safe split wrapper: {safe_visible:?}"
    );

    let (force_visible, _) = libra::command::status::changes_to_be_staged_split_force()
        .expect("force split wrapper should read worktree");
    assert_eq!(
        force_visible
            .new
            .iter()
            .any(|path| path == Path::new("foo.txt")),
        expected_alias_is_new,
        "core.ignorecase={configured_ignorecase} should control same-file case aliases in force split wrapper: {force_visible:?}"
    );
}

#[test]
fn init_records_probed_ignorecase() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let out = run_libra_command(&["config", "get", "core.ignorecase"], p);
    assert_cli_success(&out, "config get");
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // On this CI filesystem the probe answers false; on a genuinely
    // case-insensitive dev machine it answers true — either way the key is
    // PRESENT and truthful (no longer a Windows-only hard-code).
    assert!(value == "true" || value == "false", "probed value: {value}");
}

#[test]
fn mv_case_only_rename_rekeys_index() {
    let repo = case_repo();
    let p = repo.path();
    // On a case-sensitive FS this is a plain rename; on a case-insensitive
    // FS the same-inode classification + direct-rename path handles it.
    // Either way: no --force needed, index rekeys, content preserved.
    let out = run_libra_command(&["mv", "Foo.txt", "foo.txt"], p);
    assert_cli_success(&out, "case-only mv");
    assert_eq!(fs::read_to_string(p.join("foo.txt")).unwrap(), "content\n");
    let status = run_libra_command(&["--json", "status"], p);
    let json = parse_json_stdout(&status);
    let staged_new: Vec<String> = json["data"]["staged"]["new"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        staged_new.contains(&"foo.txt".to_string()),
        "rekeyed: {json}"
    );
    // Self-move still refused.
    let same = run_libra_command(&["mv", "foo.txt", "foo.txt"], p);
    assert!(!same.status.success(), "byte-equal self-move refused");
}

#[test]
fn add_refuses_case_fold_twins_under_error_default() {
    let repo = case_repo();
    let p = repo.path();
    if skip_case_twin_fixture_on_case_insensitive_host(p, "case-fold add twin") {
        return;
    }
    // Fabricate the case-insensitive view.
    assert_cli_success(
        &run_libra_command(&["config", "core.ignorecase", "true"], p),
        "force ignorecase",
    );
    // A different-cased twin of a tracked file.
    fs::write(p.join("foo.txt"), "twin\n").unwrap();
    let refused = run_libra_command(&["add", "foo.txt"], p);
    assert_eq!(refused.status.code(), Some(128), "twin refused by default");
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains("LBR-CASE-001") && err.contains("Foo.txt"),
        "{err}"
    );
    assert!(err.contains("libra mv"), "deliberate-rename hint: {err}");
    // warn mode: skipped with a warning, no index twin.
    assert_cli_success(
        &run_libra_command(&["config", "core.casehandling", "warn"], p),
        "warn mode",
    );
    let warned = run_libra_command(&["add", "foo.txt"], p);
    assert_cli_success(&warned, "warn proceeds");
    assert!(
        String::from_utf8_lossy(&warned.stderr).contains("case-fold collision"),
        "{}",
        String::from_utf8_lossy(&warned.stderr)
    );
    let ls = run_libra_command(&["ls-files"], p);
    let listing = String::from_utf8_lossy(&ls.stdout).to_string();
    assert!(
        !listing.contains("foo.txt"),
        "no index twin in any mode: {listing}"
    );
    // Invalid policy value is a hard error.
    assert_cli_success(
        &run_libra_command(&["config", "core.casehandling", "sometimes"], p),
        "bad value set",
    );
    let bad = run_libra_command(&["add", "foo.txt"], p);
    assert!(!bad.status.success(), "typo must not weaken the default");
    // Unaffected paths still stage fine under error mode.
    assert_cli_success(
        &run_libra_command(&["config", "core.casehandling", "error"], p),
        "back to error",
    );
    fs::write(p.join("other.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "other.txt"], p), "clean add");
}

#[test]
fn add_accepts_case_renamed_tracked_directory_alias() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    if !libra::utils::path_case::probe_dir_ignore_case(p) {
        eprintln!("skipping case-renamed directory add: host filesystem is case-sensitive");
        return;
    }

    fs::create_dir(p.join("slides")).unwrap();
    fs::write(p.join("slides/a.txt"), "one\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "slides/a.txt"], p),
        "add slides",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "slides", "--no-verify"], p),
        "commit slides",
    );

    fs::rename(p.join("slides"), p.join("slides-tmp")).unwrap();
    fs::rename(p.join("slides-tmp"), p.join("Slides")).unwrap();
    fs::write(p.join("Slides/a.txt"), "two\n").unwrap();

    let added = run_libra_command(&["add", "Slides"], p);
    assert_cli_success(&added, "add through case-renamed directory");

    let ls = run_libra_command(&["ls-files"], p);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("slides/a.txt"), "{listing}");
    assert!(!listing.contains("Slides/a.txt"), "{listing}");

    let status = run_libra_command(&["--json", "status"], p);
    let json = parse_json_stdout(&status);
    let staged_modified: Vec<String> = json["data"]["staged"]["modified"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(staged_modified, vec!["slides/a.txt".to_string()]);
    let unstaged_new: Vec<String> = json["data"]["unstaged"]["new"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(!unstaged_new.contains(&"Slides/".to_string()), "{json}");
}

#[test]
fn checkout_switch_refuse_colliding_trees_on_insensitive_view() {
    let repo = case_repo();
    let p = repo.path();
    if skip_case_twin_fixture_on_case_insensitive_host(p, "colliding tree checkout") {
        return;
    }
    // Build a branch whose tree carries BOTH casings (legal on ext4).
    assert_cli_success(&run_libra_command(&["branch", "twins"], p), "branch");
    assert_cli_success(&run_libra_command(&["switch", "twins"], p), "switch");
    fs::write(p.join("foo.txt"), "twin\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "foo.txt"], p), "add twin");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "twins", "--no-verify"], p),
        "commit twins",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "back to main");
    // Now pretend the FS is case-insensitive: materializing `twins` would
    // collide — the default policy refuses BEFORE any write.
    assert_cli_success(
        &run_libra_command(&["config", "core.ignorecase", "true"], p),
        "force ignorecase",
    );
    for cmd in [vec!["switch", "twins"], vec!["checkout", "twins"]] {
        let refused = run_libra_command(&cmd, p);
        assert_eq!(refused.status.code(), Some(128), "{cmd:?} refused");
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            err.contains("LBR-CASE-001") && err.contains("Foo.txt") && err.contains("foo.txt"),
            "{cmd:?}: {err}"
        );
        // Still on main, worktree intact.
        assert_eq!(
            fs::read_to_string(p.join("Foo.txt")).unwrap(),
            "content\n",
            "no partial write"
        );
    }
    // warn mode proceeds (git parity) with a warning.
    assert_cli_success(
        &run_libra_command(&["config", "core.casehandling", "warn"], p),
        "warn",
    );
    let warned = run_libra_command(&["switch", "twins"], p);
    assert_cli_success(&warned, "warn-mode switch proceeds");
    assert!(
        String::from_utf8_lossy(&warned.stderr).contains("case-fold collision"),
        "{}",
        String::from_utf8_lossy(&warned.stderr)
    );
}
