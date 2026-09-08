use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use sea_orm::{ConnectionTrait, Statement};
use serde_json::Value;
use tempfile::tempdir;

mod helpers;

use helpers::memory_cli::{assert_success, init, run, stderr_json, stdout_json};

fn object_keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("expected JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn expected_keys<const N: usize>(keys: [&str; N]) -> BTreeSet<&str> {
    keys.into_iter().collect()
}

fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).expect("read repository storage directory") {
            let entry = entry.expect("read repository storage entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("read storage entry type");
            if file_type.is_dir() {
                visit(root, &path, snapshot);
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("storage path remains under root")
                    .to_path_buf();
                snapshot.insert(relative, fs::read(path).expect("read storage file"));
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn remove_latest_schema_version(database_path: &Path) -> i64 {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build schema test runtime");
    runtime.block_on(async {
        let database = libra::internal::db::open_database_without_migrations(database_path)
            .await
            .expect("open repository database without migrations");
        let row = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT MAX(version) AS version FROM schema_versions".to_string(),
            ))
            .await
            .expect("read latest schema version")
            .expect("schema version aggregate row");
        let version = row
            .try_get::<i64>("", "version")
            .expect("decode latest schema version");
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "DELETE FROM schema_versions WHERE version = ?",
                [version.into()],
            ))
            .await
            .expect("mark latest migration pending");
        database.close().await.expect("close schema test database");
        version
    })
}

fn schema_version_exists(database_path: &Path, version: i64) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build schema verification runtime");
    runtime.block_on(async {
        let database = libra::internal::db::open_database_without_migrations(database_path)
            .await
            .expect("open repository database without migrations");
        let row = database
            .query_one_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "SELECT 1 AS present FROM schema_versions WHERE version = ? LIMIT 1",
                [version.into()],
            ))
            .await
            .expect("inspect schema version");
        database
            .close()
            .await
            .expect("close schema verification database");
        row.is_some()
    })
}

fn insert_future_schema_version(database_path: &Path) -> i64 {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build future-schema test runtime");
    runtime.block_on(async {
        let database = libra::internal::db::open_database_without_migrations(database_path)
            .await
            .expect("open repository database without migrations");
        let row = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT MAX(version) AS version FROM schema_versions".to_string(),
            ))
            .await
            .expect("read latest schema version")
            .expect("schema version aggregate row");
        let future_version = row
            .try_get::<i64>("", "version")
            .expect("decode latest schema version")
            + 1_000;
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                [
                    future_version.into(),
                    "future-memory-test".into(),
                    0_i64.into(),
                ],
            ))
            .await
            .expect("insert future schema marker");
        database.close().await.expect("close schema test database");
        future_version
    })
}

#[test]
fn cli_json_schema_and_errors() {
    let repository = tempdir().expect("create Memory CLI repository");
    init(repository.path());

    let status = run(&["--json", "memory", "status"], repository.path());
    assert_success(&status, "read Memory status");
    let status = stdout_json(&status);
    assert_eq!(
        object_keys(&status),
        expected_keys(["command", "data", "ok"])
    );
    assert_eq!(status["ok"], true);
    assert_eq!(status["command"], "memory.status");
    assert_eq!(
        object_keys(&status["data"]),
        expected_keys([
            "digest_key_available",
            "fts5_enabled",
            "jobs",
            "last_event_seq",
            "memory_ref",
            "projected_ref",
            "projection_head",
            "projection_state",
            "view_hash",
        ])
    );
    assert_eq!(status["data"]["projection_state"], "empty");
    assert_eq!(status["data"]["jobs"]["scan_limit"], 4096);
    assert_eq!(status["data"]["jobs"]["truncated"], false);
    assert_eq!(
        object_keys(&status["data"]["jobs"]),
        expected_keys([
            "active_leases",
            "dirty",
            "error_count",
            "expired_leases",
            "failed",
            "idle",
            "inflight",
            "pending_generations",
            "retry_count",
            "scan_limit",
            "total",
            "truncated",
        ])
    );

    let search = run(
        &["--json", "memory", "search", "authentication retry"],
        repository.path(),
    );
    assert_success(&search, "search empty Memory");
    let search = stdout_json(&search);
    assert_eq!(search["command"], "memory.search");
    assert_eq!(
        object_keys(&search["data"]),
        expected_keys([
            "candidates_examined",
            "items",
            "omitted_by_applicability",
            "relation_omissions",
            "selector_limit_omissions",
            "selector_version",
            "view_hash",
        ])
    );
    assert_eq!(search["data"]["items"], serde_json::json!([]));

    for invalid_query in ["", "!!!"] {
        let invalid = run(
            &["--json", "memory", "search", invalid_query],
            repository.path(),
        );
        assert!(
            !invalid.status.success(),
            "invalid query {invalid_query:?} must fail even when Memory is empty"
        );
        let report = stderr_json(&invalid);
        assert_eq!(report["error_code"], "LBR-MEMORY-QUERY-INVALID");
    }
    let oversized_query = "x".repeat(4 * 1024 + 1);
    let invalid = run(
        &["--json", "memory", "search", &oversized_query],
        repository.path(),
    );
    assert!(!invalid.status.success(), "oversized query must fail");
    let report = stderr_json(&invalid);
    assert_eq!(report["error_code"], "LBR-MEMORY-QUERY-INVALID");

    let invalid = run(
        &["--json", "memory", "show", "not-a-note-id"],
        repository.path(),
    );
    assert!(!invalid.status.success());
    let invalid = stderr_json(&invalid);
    assert_eq!(invalid["error_code"], "LBR-MEMORY-QUERY-INVALID");
    assert_eq!(invalid["category"], "cli");
    assert_eq!(invalid["exit_code"], 129);

    for args in [
        vec!["--json", "memory", "search", "retry", "--limit", "0"],
        vec![
            "--json",
            "memory",
            "search",
            "retry",
            "--completion",
            "pending",
        ],
        vec!["--json", "memory", "search", "retry", "--root-kind", "task"],
        vec![
            "--json",
            "memory",
            "search",
            "retry",
            "--path",
            "episodic.tasks",
            "--path-prefix",
            "episodic",
        ],
        vec![
            "--json",
            "memory",
            "search",
            "retry",
            "--ended-from",
            "yesterday",
        ],
    ] {
        let invalid_filter = run(&args, repository.path());
        assert!(!invalid_filter.status.success(), "{args:?} must fail");
        let report = stderr_json(&invalid_filter);
        assert_eq!(report["error_code"], "LBR-MEMORY-QUERY-INVALID");
        assert_eq!(report["category"], "cli");
    }

    let missing = run(
        &[
            "--json",
            "memory",
            "show",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        repository.path(),
    );
    assert!(!missing.status.success());
    let missing = stderr_json(&missing);
    assert_eq!(missing["error_code"], "LBR-MEMORY-NOT-FOUND");
    assert_eq!(missing["category"], "repo");
}

#[test]
fn future_schema_uses_memory_error_contract() {
    let repository = tempdir().expect("create future-schema Memory repository");
    init(repository.path());
    let database_path = repository.path().join(".libra").join("libra.db");
    let future_version = insert_future_schema_version(&database_path);

    for args in [
        vec!["--json", "memory", "status"],
        vec!["--json", "memory", "rebuild"],
    ] {
        let output = run(&args, repository.path());
        assert!(
            !output.status.success(),
            "future schema must fail: {args:?}"
        );
        let report = stderr_json(&output);
        assert_eq!(report["error_code"], "LBR-MEMORY-002");
        assert_eq!(report["category"], "repo");
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|message| message.contains(&future_version.to_string())),
            "future version should be actionable: {report}"
        );
        assert!(
            report["hints"]
                .as_array()
                .is_some_and(|hints| hints.iter().any(|hint| {
                    hint.as_str()
                        .is_some_and(|hint| hint.contains("upgrade Libra"))
                })),
            "future schema should direct the user to upgrade Libra: {report}"
        );
    }
}

#[test]
fn rebuild_dry_run_zero_writes() {
    let repository = tempdir().expect("create Memory rebuild repository");
    init(repository.path());

    let storage = repository.path().join(".libra");
    let database_path = storage.join("libra.db");
    let pending_version = remove_latest_schema_version(&database_path);
    assert!(!schema_version_exists(&database_path, pending_version));
    let before = snapshot_files(&storage);

    let dry_run = run(
        &["--json", "memory", "rebuild", "--dry-run"],
        repository.path(),
    );
    assert_success(&dry_run, "validate Memory rebuild");
    let report = stdout_json(&dry_run);
    assert_eq!(report["command"], "memory.rebuild");
    assert_eq!(report["data"]["dry_run"], true);
    assert_eq!(report["data"]["changed"], false);
    assert_eq!(report["data"]["head"], Value::Null);
    assert_eq!(report["data"]["event_count"], 0);
    assert_eq!(report["data"]["note_count"], 0);
    assert_eq!(report["data"]["revision_count"], 0);

    let after = snapshot_files(&storage);
    assert_eq!(
        after, before,
        "Memory rebuild --dry-run must write zero bytes"
    );
    assert!(
        !schema_version_exists(&database_path, pending_version),
        "Memory rebuild --dry-run must not apply pending migrations"
    );
}
