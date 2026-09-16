//! Structured JSON output tests for `libra status`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use libra::{
    internal::{branch::Branch, head::Head},
    utils::test::ChangeDirGuard,
};
use serial_test::serial;
use tempfile::tempdir;

use super::{assert_cli_success, configure_identity_via_cli, init_repo_via_cli, run_libra_command};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_json_stdout(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nerror: {e}"))
}

fn create_committed_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("tracked.txt"), "tracked\n").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "tracked.txt"], repo.path());
    assert_cli_success(&out, "add base files");
    let out = run_libra_command(&["commit", "-m", "initial", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    repo
}

// ---------------------------------------------------------------------------
// Schema completeness — clean repo
// ---------------------------------------------------------------------------

#[test]
fn json_status_clean_repo_schema() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status clean");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "status");

    let data = &parsed["data"];
    // head
    let head = &data["head"];
    assert_eq!(head["type"].as_str(), Some("branch"));
    assert!(head["name"].is_string());

    // has_commits
    assert_eq!(data["has_commits"], true);

    // upstream is null when not configured
    assert!(
        data["upstream"].is_null(),
        "upstream should be null without remote config"
    );

    // staged
    assert!(data["staged"]["new"].is_array());
    assert!(data["staged"]["modified"].is_array());
    assert!(data["staged"]["deleted"].is_array());

    // unstaged
    assert!(data["unstaged"]["modified"].is_array());
    assert!(data["unstaged"]["deleted"].is_array());

    // untracked, ignored
    assert!(data["untracked"].is_array());
    assert!(data["ignored"].is_array());

    // is_clean
    assert_eq!(data["is_clean"], true);
}

#[tokio::test]
#[serial(cwd)]
async fn json_status_includes_upstream_tracking_info() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["config", "branch.main.remote", "origin"], repo.path());
    assert_cli_success(&output, "configure branch.main.remote");
    let output = run_libra_command(
        &["config", "branch.main.merge", "refs/heads/main"],
        repo.path(),
    );
    assert_cli_success(&output, "configure branch.main.merge");

    let _guard = ChangeDirGuard::new(repo.path());
    let head = Head::current_commit().await.expect("head commit");
    Branch::update_branch("main", &head.to_string(), Some("origin"))
        .await
        .expect("create remote-tracking branch");

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status upstream");

    let parsed = parse_json_stdout(&output);
    let upstream = &parsed["data"]["upstream"];
    assert_eq!(upstream["remote_ref"], "origin/main");
    assert_eq!(upstream["ahead"], 0);
    assert_eq!(upstream["behind"], 0);
    assert_eq!(upstream["gone"], false);
}

/// Issue #464 regression: clone/fetch/push store the tracking ref under its
/// fully-qualified `refs/remotes/<remote>/<branch>` name, so a fresh clone
/// must report a healthy upstream (ahead/behind), not `gone: true`.
#[tokio::test]
#[serial(cwd)]
async fn json_status_resolves_fully_qualified_tracking_ref() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["config", "branch.main.remote", "origin"], repo.path());
    assert_cli_success(&output, "configure branch.main.remote");
    let output = run_libra_command(
        &["config", "branch.main.merge", "refs/heads/main"],
        repo.path(),
    );
    assert_cli_success(&output, "configure branch.main.merge");

    let _guard = ChangeDirGuard::new(repo.path());
    let head = Head::current_commit().await.expect("head commit");
    // Same shape clone/fetch/push write: fully-qualified name + remote column.
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .expect("create fully-qualified remote-tracking branch");

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status fully-qualified upstream");

    let parsed = parse_json_stdout(&output);
    let upstream = &parsed["data"]["upstream"];
    assert_eq!(upstream["remote_ref"], "origin/main");
    assert_eq!(
        upstream["gone"], false,
        "fully-qualified tracking ref must not be reported as gone"
    );
    assert_eq!(upstream["ahead"], 0);
    assert_eq!(upstream["behind"], 0);
}

/// When BOTH naming conventions exist (a legacy short row left by an older
/// binary plus a fresh fully-qualified row written by push/fetch), the
/// fully-qualified row must win — it is the one current writers keep current.
#[tokio::test]
#[serial(cwd)]
async fn json_status_prefers_fully_qualified_row_over_legacy_short_row() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["config", "branch.main.remote", "origin"], repo.path());
    assert_cli_success(&output, "configure branch.main.remote");
    let output = run_libra_command(
        &["config", "branch.main.merge", "refs/heads/main"],
        repo.path(),
    );
    assert_cli_success(&output, "configure branch.main.merge");

    let _guard = ChangeDirGuard::new(repo.path());
    // A second commit so the tracking rows can be pointed at different tips.
    fs::write(repo.path().join("tracked.txt"), "second\n").unwrap();
    let output = run_libra_command(
        &["commit", "-a", "-m", "second", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "second commit");
    let head = Head::current_commit().await.expect("head commit");
    // Diverge the two rows: the legacy short row points at HEAD, the
    // fully-qualified row at its parent (as if the remote moved HEAD).
    Branch::update_branch("main", &head.to_string(), Some("origin"))
        .await
        .expect("create legacy short remote-tracking branch");
    let parent = load_parent_commit_oid(&head);
    Branch::update_branch("refs/remotes/origin/main", &parent, Some("origin"))
        .await
        .expect("create fully-qualified remote-tracking branch");

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status dual tracking rows");

    let parsed = parse_json_stdout(&output);
    let upstream = &parsed["data"]["upstream"];
    assert_eq!(upstream["gone"], false, "fully-qualified row must be found");
    // Short row == HEAD → the short row would report ahead 0/behind 0. The
    // full row points at HEAD's parent, so ahead=1 pins that the FULLY
    // qualified row won the lookup.
    assert_eq!(
        upstream["ahead"], 1,
        "status must use the fully-qualified row's tip, not the legacy short row"
    );
    assert_eq!(upstream["behind"], 0);
}

fn load_parent_commit_oid(commit: &ObjectHash) -> String {
    use libra::utils::object_ext::CommitExt;
    let loaded = Commit::try_load(commit).expect("load head commit");
    let parent = loaded
        .parent_commit_ids
        .first()
        .copied()
        .expect("head commit should have a parent");
    parent.to_string()
}

// ---------------------------------------------------------------------------
// Dirty repo
// ---------------------------------------------------------------------------

#[test]
fn json_status_dirty_repo() {
    let repo = create_committed_repo();

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    fs::write(repo.path().join("untracked.txt"), "new\n").unwrap();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status dirty");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    assert_eq!(data["is_clean"], false);

    // unstaged modified
    let unstaged_modified: Vec<&str> = data["unstaged"]["modified"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        unstaged_modified.contains(&"tracked.txt"),
        "unstaged modified: {unstaged_modified:?}"
    );

    // untracked
    let untracked: Vec<&str> = data["untracked"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        untracked.contains(&"untracked.txt"),
        "untracked: {untracked:?}"
    );
}

// ---------------------------------------------------------------------------
// Staged changes in JSON
// ---------------------------------------------------------------------------

#[test]
fn json_status_with_staged_changes() {
    let repo = create_committed_repo();

    fs::write(repo.path().join("new_file.rs"), "fn main() {}").unwrap();
    let out = run_libra_command(&["add", "new_file.rs"], repo.path());
    assert_cli_success(&out, "add new_file.rs");

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status staged");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    let staged_new: Vec<&str> = data["staged"]["new"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        staged_new.contains(&"new_file.rs"),
        "staged new: {staged_new:?}"
    );
    assert_eq!(data["is_clean"], false);
}

// ---------------------------------------------------------------------------
// --show-stash --json
// ---------------------------------------------------------------------------

#[test]
fn json_status_no_stash_entries_field_by_default() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status no stash");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    // stash_entries should not be present without --show-stash
    let data_obj = data.as_object().expect("data should be a JSON object");
    assert!(
        !data_obj.contains_key("stash_entries"),
        "stash_entries key should be absent by default, got: {data}"
    );
}

#[test]
fn json_status_stash_entries_present_with_show_stash_even_when_zero() {
    // docs/commands/status.md promises that `stash_entries` is present iff
    // `--show-stash` is passed, and may legitimately be `0`. Pinning the
    // opt-in semantics here so a regression cannot silently couple the
    // field's presence to "at least one stash exists".
    let repo = create_committed_repo();

    let output = run_libra_command(&["--json", "status", "--show-stash"], repo.path());
    assert_cli_success(&output, "json status --show-stash");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    let data_obj = data.as_object().expect("data should be a JSON object");
    assert!(
        data_obj.contains_key("stash_entries"),
        "stash_entries key should be present with --show-stash, got: {data}"
    );
    assert_eq!(
        data["stash_entries"], 0,
        "fresh repo should report zero stash entries, got: {}",
        data["stash_entries"]
    );
}

// ---------------------------------------------------------------------------
// No commits yet
// ---------------------------------------------------------------------------

#[test]
fn json_status_no_commits() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("new.txt"), "new").unwrap();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status no commits");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    assert_eq!(data["has_commits"], false);
}

// ---------------------------------------------------------------------------
// Backward compatibility: existing fields unchanged
// ---------------------------------------------------------------------------

#[test]
fn json_status_backward_compat_field_types() {
    let repo = create_committed_repo();
    fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status backward compat");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];

    // Verify all existing fields exist with correct types
    assert!(data["head"].is_object(), "head should be object");
    assert!(
        data["has_commits"].is_boolean(),
        "has_commits should be bool"
    );
    assert!(data["staged"].is_object(), "staged should be object");
    assert!(data["unstaged"].is_object(), "unstaged should be object");
    assert!(data["untracked"].is_array(), "untracked should be array");
    assert!(data["ignored"].is_array(), "ignored should be array");
    assert!(data["is_clean"].is_boolean(), "is_clean should be bool");
}

// ---------------------------------------------------------------------------
// --machine produces single-line JSON
// ---------------------------------------------------------------------------

#[test]
fn machine_status_is_single_line_json() {
    let repo = create_committed_repo();

    let output = run_libra_command(&["--machine", "status"], repo.path());
    assert_cli_success(&output, "machine status");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<_> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly 1 line, got: {non_empty_lines:?}"
    );
    let _: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
}

// ---------------------------------------------------------------------------
// Paths are relative
// ---------------------------------------------------------------------------

#[test]
fn json_status_paths_are_relative() {
    let repo = create_committed_repo();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/lib.rs"), "pub fn foo() {}").unwrap();

    let output = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&output, "json status paths");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];

    // Check untracked paths are relative
    for path_val in data["untracked"].as_array().unwrap() {
        let s = path_val.as_str().unwrap();
        assert!(!s.starts_with('/'), "path should be relative: {s}");
    }
}

// ---------------------------------------------------------------------------
// Upstream ahead/behind counts (#486)
// ---------------------------------------------------------------------------

fn json_upstream(repo: &std::path::Path) -> serde_json::Value {
    let output = run_libra_command(&["--json", "status"], repo);
    assert_cli_success(&output, "json status");
    parse_json_stdout(&output)
}

/// `data.warnings[]` holds exactly one `upstream_counts_unavailable` warning.
fn assert_upstream_count_warning(envelope: &serde_json::Value) {
    let warnings = envelope["data"]["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("data.warnings[] is an array: {envelope}"));
    let matching: Vec<_> = warnings
        .iter()
        .filter(|warning| warning["code"] == "upstream_counts_unavailable")
        .collect();
    assert_eq!(matching.len(), 1, "one upstream-count warning: {envelope}");
    assert_eq!(matching[0]["source"], "metadata", "{envelope}");
    assert!(
        matching[0]["message"].as_str().is_some_and(
            |message| message.starts_with("cannot count commits ahead/behind 'origin/main'")
        ),
        "{envelope}"
    );
}

#[tokio::test]
#[serial(cwd)]
/// M-RENDER R4, R1, R2 and R3 in the JSON `upstream` object.
async fn json_status_upstream_counts_shapes() {
    use super::status_test::{
        commit_named_file, rev_parse, upstream_tracking_repo, write_upstream_ref,
    };

    let repo = upstream_tracking_repo(5);
    let p = repo.path();
    let _cwd = ChangeDirGuard::new(p);
    let counts = |parsed: &serde_json::Value| {
        let upstream = &parsed["data"]["upstream"];
        assert_eq!(upstream["gone"], false, "{parsed}");
        (upstream["ahead"].clone(), upstream["behind"].clone())
    };

    write_upstream_ref(&rev_parse(p, "HEAD")).await;
    assert_eq!(
        counts(&json_upstream(p)),
        (serde_json::json!(0), serde_json::json!(0))
    );

    write_upstream_ref(&rev_parse(p, "HEAD~1")).await;
    assert_eq!(
        counts(&json_upstream(p)),
        (serde_json::json!(1), serde_json::json!(0))
    );

    write_upstream_ref(&rev_parse(p, "HEAD")).await;
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD~2"], p),
        "reset --hard HEAD~2",
    );
    assert_eq!(
        counts(&json_upstream(p)),
        (serde_json::json!(0), serde_json::json!(2))
    );

    commit_named_file(p, "l1");
    assert_eq!(
        counts(&json_upstream(p)),
        (serde_json::json!(1), serde_json::json!(2))
    );
}

#[tokio::test]
#[serial(cwd)]
/// M-UNKNOWN U2 and U3 in JSON: `ahead`/`behind` are `null` with `gone: false`;
/// only the unreadable history carries a warning.
async fn json_status_upstream_counts_unavailable_are_null() {
    use super::{
        loose_object_path,
        status_test::{rev_parse, upstream_tracking_repo, write_upstream_ref},
    };

    {
        let repo = upstream_tracking_repo(5);
        let p = repo.path();
        let _cwd = ChangeDirGuard::new(p);
        write_upstream_ref(&rev_parse(p, "HEAD~1")).await;
        fs::remove_file(loose_object_path(p, &rev_parse(p, "HEAD~3")))
            .expect("remove a shared commit");
        let output = run_libra_command(&["--json", "status"], p);
        assert_cli_success(&output, "json status");
        assert!(
            output.stderr.is_empty(),
            "JSON mode keeps stderr clean: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let parsed = parse_json_stdout(&output);
        let upstream = &parsed["data"]["upstream"];
        assert_eq!(upstream["gone"], false, "{parsed}");
        assert!(upstream["ahead"].is_null(), "{parsed}");
        assert!(upstream["behind"].is_null(), "{parsed}");
        assert_upstream_count_warning(&parsed);
    }

    let repo = upstream_tracking_repo(1);
    let p = repo.path();
    let _cwd = ChangeDirGuard::new(p);
    write_upstream_ref(&rev_parse(p, "HEAD")).await;
    assert_cli_success(
        &run_libra_command(&["switch", "--orphan", "fresh"], p),
        "switch --orphan fresh",
    );
    for (key, value) in [
        ("branch.fresh.remote", "origin"),
        ("branch.fresh.merge", "refs/heads/main"),
    ] {
        assert_cli_success(&run_libra_command(&["config", key, value], p), key);
    }
    let parsed = json_upstream(p);
    let upstream = &parsed["data"]["upstream"];
    assert_eq!(upstream["gone"], false, "{parsed}");
    assert!(upstream["ahead"].is_null(), "{parsed}");
    assert!(upstream["behind"].is_null(), "{parsed}");
    assert!(
        parsed["data"]["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.is_empty()),
        "an unborn branch is not an unavailable count: {parsed}"
    );
}

#[tokio::test]
#[serial(cwd)]
/// M-UNKNOWN U2 on the other delivery paths: the dirty-cache `--cached` mode,
/// `--exit-code-on-warning` under `--json`, and the embedding API — which
/// carries the warning in its own envelope without touching the process
/// warning tracker.
async fn json_status_upstream_counts_unavailable_cached_and_api() {
    use super::{
        loose_object_path,
        status_test::{rev_parse, upstream_tracking_repo, write_upstream_ref},
    };

    let repo = upstream_tracking_repo(5);
    let p = repo.path();
    let _cwd = ChangeDirGuard::new(p);
    write_upstream_ref(&rev_parse(p, "HEAD~1")).await;
    fs::remove_file(loose_object_path(p, &rev_parse(p, "HEAD~3"))).expect("remove a shared commit");

    assert_cli_success(
        &run_libra_command(&["status", "--scan"], p),
        "status --scan",
    );
    let cached = run_libra_command(&["--json", "status", "--cached"], p);
    assert_cli_success(&cached, "json status --cached");
    let parsed = parse_json_stdout(&cached);
    assert_eq!(parsed["data"]["freshness"], "cached", "{parsed}");
    assert!(parsed["data"]["upstream"]["ahead"].is_null(), "{parsed}");
    assert_upstream_count_warning(&parsed);

    let gated = run_libra_command(&["--json", "--exit-code-on-warning", "status"], p);
    assert_eq!(
        gated.status.code(),
        Some(9),
        "the warning drives exit 9: {}",
        String::from_utf8_lossy(&gated.stderr)
    );
    assert_upstream_count_warning(&parse_json_stdout(&gated));

    libra::utils::output::reset_warning_tracker();
    assert!(!libra::utils::output::warning_was_emitted());
    let envelope = libra::command::status::collect_status_json_envelope_for_api(p)
        .await
        .expect("api status");
    assert_upstream_count_warning(&envelope);
    assert!(
        !libra::utils::output::warning_was_emitted(),
        "the API path must not touch the process warning tracker"
    );
}
