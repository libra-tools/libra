//! Integration tests for `libra memory` (plan-20260926 DM-03 registration and
//! DM-11 read-only subcommands + stable error codes + fail-closed semantics).

use super::*;

fn memory_repo() -> tempfile::TempDir {
    create_committed_repo_via_cli()
}

/// A `libra memory` subcommand is read-only and must not write an `operation`
/// row (AC memory_writes_no_operation_row).
#[tokio::test]
async fn memory_writes_no_operation_row() {
    let repo = memory_repo();
    let p = repo.path();
    let db_path = p.join(".libra/libra.db");
    let conn = libra::internal::db::establish_connection(db_path.to_str().unwrap())
        .await
        .expect("open repo DB");

    async fn operation_count<C: sea_orm::ConnectionTrait>(conn: &C) -> i64 {
        use sea_orm::Statement;
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS c FROM operation",
            ))
            .await
            .unwrap()
            .expect("count row");
        row.try_get("", "c").expect("count value")
    }

    let before = operation_count(&conn).await;
    let run = run_libra_command(&["memory", "status", "--allow-stale"], p);
    assert_cli_success(&run, "memory status");
    let after = operation_count(&conn).await;
    assert_eq!(
        before, after,
        "`libra memory status` must not write an operation row"
    );
}

/// The `memory` command parses and dispatches its placeholder subcommands.
#[test]
fn memory_subcommands_parse_and_run() {
    let repo = memory_repo();
    let p = repo.path();
    for sub in ["status", "list", "rebuild"] {
        let run = run_libra_command(&["memory", sub], p);
        assert_cli_success(&run, &format!("memory {sub}"));
    }
    let show = run_libra_command(&["memory", "show", "abcd"], p);
    assert!(!show.status.success(), "`memory show <missing>` must fail");
}

// ---------------------------------------------------------------------------
// plan-20260926 DM-11: read-only subcommands, stable error codes, fail-closed.
// ---------------------------------------------------------------------------

/// Run `libra memory rebuild` and assert success.
fn rebuild_projection(p: &Path) {
    let run = run_libra_command(&["memory", "rebuild"], p);
    assert_cli_success(&run, "memory rebuild");
}

/// Create a second commit in the repo, which makes the projection stale.
fn add_second_commit(p: &Path) {
    let file = p.join("second.txt");
    std::fs::write(&file, "second\n").expect("write second file");
    let add = run_libra_command(&["add", "second.txt"], p);
    assert_cli_success(&add, "add second file");
    let commit = run_libra_command(&["commit", "-m", "second", "--no-verify"], p);
    assert_cli_success(&commit, "commit second");
}

/// AC status_envelope_is_frozen: `memory status --json` derives every envelope
/// field from `memory_projection_state`, and the envelope is stable across a
/// rebuild (GC-DM-01). After a rebuild the projection is fresh, so `stale` is
/// `false` and the persisted schema/rules/horizon/count columns are surfaced.
#[test]
fn status_envelope_is_frozen() {
    let repo = memory_repo();
    let p = repo.path();
    rebuild_projection(p);

    let run = run_libra_command(&["--json", "memory", "status"], p);
    assert_cli_success(&run, "memory status --json after rebuild");
    let envelope = parse_json_stdout(&run);
    let data = envelope
        .get("data")
        .expect("envelope has data")
        .as_object()
        .expect("data is an object");
    // The frozen read contract (DM-11) — these keys must be present.
    for key in [
        "schema_version",
        "stale",
        "selector_version",
        "rules_version",
        "horizon_truncated",
        "revoked_count",
        "aged_out_count",
        "rebuilt_at",
    ] {
        assert!(data.contains_key(key), "status envelope missing '{key}'");
    }
    assert_eq!(
        data["selector_version"].as_i64(),
        Some(1),
        "selector_version frozen to 1"
    );
    assert_eq!(
        data["schema_version"].as_i64(),
        Some(1),
        "schema_version from projection state"
    );
    assert_eq!(
        data["rules_version"].as_i64(),
        Some(1),
        "rules_version from projection state"
    );
    assert_eq!(data["stale"].as_bool(), Some(false), "fresh after rebuild");
}

/// AC stale_projection_fails_closed: an outdated projection refuses to answer
/// with `LBR-MEMORY-001` unless `--allow-stale`, which marks `stale: true`.
#[test]
fn stale_projection_fails_closed() {
    let repo = memory_repo();
    let p = repo.path();
    rebuild_projection(p);
    add_second_commit(p);

    // Without --allow-stale, status fails closed with LBR-MEMORY-001 (exit 128).
    let run = run_libra_command(&["memory", "status"], p);
    assert!(!run.status.success(), "stale status must not succeed");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("LBR-MEMORY-001"),
        "stale status must emit LBR-MEMORY-001, got: {stderr}"
    );

    // The read commands (list / show) fail closed the same way.
    let list = run_libra_command(&["memory", "list"], p);
    assert!(!list.status.success(), "stale list must not succeed");
    let show = run_libra_command(&["memory", "show", "abcd"], p);
    assert!(!show.status.success(), "stale show must not succeed");

    // --allow-stale reads anyway and marks stale=true in the --json envelope.
    let allowed = run_libra_command(&["--json", "memory", "status", "--allow-stale"], p);
    assert_cli_success(&allowed, "memory status --allow-stale");
    let envelope = parse_json_stdout(&allowed);
    assert_eq!(
        envelope["data"]["stale"].as_bool(),
        Some(true),
        "allow-stale must mark stale=true"
    );
}

/// AC list_json_returns_window_episodes, show_missing_episode_is_lbr_memory_002
/// and rebuild_matches_gc_dm_01. Rebuild derives the window episodes; `list`
/// returns only window episodes; `show` returns a known episode and refuses an
/// unknown id with `LBR-MEMORY-002`.
#[test]
fn list_show_rebuild_roundtrip() {
    let repo = memory_repo();
    let p = repo.path();
    rebuild_projection(p);

    let list = run_libra_command(&["--json", "memory", "list"], p);
    assert_cli_success(&list, "memory list --json");
    let envelope = parse_json_stdout(&list);
    let episodes = envelope["data"]["episodes"]
        .as_array()
        .expect("list data.episodes is an array");
    assert!(
        !episodes.is_empty(),
        "window must contain at least one episode"
    );
    let first = episodes[0].as_object().expect("episode object");
    let episode_id = first["episode_id"].as_str().expect("episode_id string");

    // show a known episode.
    let show = run_libra_command(&["--json", "memory", "show", episode_id], p);
    assert_cli_success(&show, "memory show --json known id");
    let show_env = parse_json_stdout(&show);
    assert_eq!(
        show_env["data"]["episode_id"].as_str(),
        Some(episode_id),
        "show returns the requested episode"
    );

    // show an unknown episode id -> LBR-MEMORY-002.
    let missing = run_libra_command(
        &["memory", "show", "00000000-0000-0000-0000-000000000000"],
        p,
    );
    assert!(!missing.status.success(), "show unknown id must fail");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("LBR-MEMORY-002"),
        "show unknown id must emit LBR-MEMORY-002, got: {stderr}"
    );
}

/// Parse the stdout of a `--json` command invocation into a `serde_json::Value`.
fn parse_json_stdout(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not valid JSON: {error}\n---\n{stdout}\n---\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
