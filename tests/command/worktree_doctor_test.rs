//! `libra worktree doctor` — the Part C W4 read-only scope-diagnosis machine
//! interface (plan-20260714 §C.8 / §C.13).
//!
//! Four contracts are pinned here:
//!
//! 1. The default invocation is STRICTLY READ-ONLY — rows, registry, lease and
//!    filesystem are byte-identical before and after.
//! 2. The frozen response shapes: a paginated `data.diagnostics[]` +
//!    `data.next_cursor` view, and a singular `data.diagnostic` view with NO
//!    pagination keys, both under `command = "worktree.doctor"`.
//! 3. Keyset pagination with an OPAQUE cursor (default limit 50, cap 500) and
//!    a fail-closed `LBR-WORKTREE-001` on a cursor this command did not issue.
//! 4. Fail-closed diagnosis: an unreadable scope is `LBR-WORKTREE-002`, never
//!    a silently truncated report; and mixing the single-scope id with
//!    `--limit`/`--cursor` is a usage error (`LBR-CLI-002`).

use std::path::Path;

use libra::internal::workspace::{AcquireRequest, WorkspaceStore, now_ms};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseBackend, Statement};

use super::*;

async fn repo_db(repo: &Path) -> sea_orm::DatabaseConnection {
    let url = format!("sqlite://{}", repo.join(".libra/libra.db").display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    Database::connect(opts).await.expect("open repo db")
}

/// This repository's canonical identity, so seeded rows are attributed to it
/// (a different value would be reported as a foreign-identity record).
async fn repo_identity(conn: &sea_orm::DatabaseConnection) -> String {
    let row = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1",
        ))
        .await
        .expect("query repo identity")
        .expect("repository identity row");
    row.try_get_by_index::<String>(0).expect("identity value")
}

/// Acquire one linked workspace whose directory exists on disk.
/// `acquired_at_ms` drives the lease deadline, so a caller can seed either a
/// live lease or one that expired long ago without sleeping.
async fn seed_linked_workspace(repo: &Path, name: &str, acquired_at_ms: i64) -> String {
    let conn = repo_db(repo).await;
    let dir = repo.join(name);
    fs::create_dir_all(&dir).expect("create workspace dir");
    let request = AcquireRequest::linked(
        format!("wt-{name}"),
        dir.canonicalize().expect("canonical workspace dir"),
        format!("agent-{name}"),
        600_000,
    );
    let lease = WorkspaceStore::acquire(&conn, &request, acquired_at_ms)
        .await
        .expect("acquire linked workspace");
    lease.workspace_id
}

/// Insert `count` plain rows straight through SQL — the only practical way to
/// prove the 500-row page cap without minutes of real acquisitions.
async fn bulk_seed(conn: &sea_orm::DatabaseConnection, repo_id: &str, count: usize) {
    for index in 0..count {
        conn.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO workspace_record (workspace_id, repo_id, kind, worktree_id, path, \
             owner_kind, state, lease_fence, created_at, updated_at) \
             VALUES (?, ?, 'remote', NULL, ?, 'agent', 'active', 0, 1, 1)",
            [
                format!("bulk-{index:04}").into(),
                repo_id.into(),
                format!("/nonexistent/bulk-{index:04}").into(),
            ],
        ))
        .await
        .expect("bulk insert workspace row");
    }
}

/// Everything the read-only contract covers: every `workspace_record` row, the
/// worktree registry bytes, and the repository's file tree.
///
/// Two families are filtered out of the tree listing because ANY command that
/// merely opens the repository creates them — `libra worktree list`, a plain
/// reader, creates both — so including them would make the assertion about
/// process startup rather than about this command writing repository state:
///
/// * SQLite's transient sidecars (`-wal`, `-shm`, `-journal`);
/// * the object-index repair advisory lock directory (`.lock` files), which
///   holds no repository state at all.
async fn repo_snapshot(repo: &Path) -> (Vec<String>, Vec<u8>, Vec<String>) {
    let conn = repo_db(repo).await;
    let rows = conn
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT workspace_id || '|' || repo_id || '|' || kind || '|' || path || '|' || \
             state || '|' || COALESCE(lease_owner, '') || '|' || lease_fence || '|' || \
             COALESCE(lease_expires_at, -1) || '|' || updated_at AS row FROM workspace_record \
             ORDER BY workspace_id",
        ))
        .await
        .expect("dump workspace rows");
    let rows: Vec<String> = rows
        .iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("row text"))
        .collect();

    let registry = fs::read(repo.join(".libra/worktrees.json")).unwrap_or_default();

    let mut tree = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.to_string_lossy().to_string();
            if name.ends_with("-wal")
                || name.ends_with("-shm")
                || name.ends_with("-journal")
                || name.ends_with(".lock")
                || name.ends_with("object-index-repair-locks")
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path.clone());
            }
            tree.push(name);
        }
    }
    tree.sort();
    (rows, registry, tree)
}

fn parse_json(bytes: &[u8], what: &str) -> serde_json::Value {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(text.trim()).unwrap_or_else(|error| panic!("parse {what} json: {error}"))
}

/// The doctor reports what is wrong with each workspace scope — and changes
/// nothing while doing so (§C.8 W4 acceptance: default invocation is strictly
/// read-only).
#[tokio::test]
#[serial]
async fn worktree_doctor_reports_scope_diagnostics() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());

    // A healthy, currently-leased workspace...
    let healthy = seed_linked_workspace(repo.path(), "live", now_ms()).await;
    // ...and one whose lease expired long ago and whose directory is gone.
    let broken = seed_linked_workspace(repo.path(), "stale", 1_000).await;
    fs::remove_dir_all(repo.path().join("stale")).expect("drop the stale workspace directory");

    let before = repo_snapshot(repo.path()).await;
    let output = run_libra_command(&["--json", "worktree", "doctor"], repo.path());
    assert_cli_success(&output, "worktree doctor");
    let after = repo_snapshot(repo.path()).await;
    assert_eq!(
        before, after,
        "the default doctor invocation must not write"
    );

    let doc = parse_json(&output.stdout, "doctor page");
    assert_eq!(doc["ok"], true, "{doc}");
    assert_eq!(doc["command"], "worktree.doctor", "{doc}");
    let mut page_keys: Vec<&str> = doc["data"]
        .as_object()
        .expect("doctor page data object")
        .keys()
        .map(String::as_str)
        .collect();
    page_keys.sort_unstable();
    assert_eq!(
        page_keys,
        vec!["diagnostics", "next_cursor", "schema_version"],
        "the versioned page shape must not grow unreviewed fields: {doc}"
    );
    assert_eq!(doc["data"]["schema_version"], 1, "{doc}");
    let diagnostics = doc["data"]["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(diagnostics.len(), 2, "{doc}");

    let find = |id: &str| {
        diagnostics
            .iter()
            .find(|item| item["workspace_id"] == id)
            .unwrap_or_else(|| panic!("diagnostic for {id} missing: {doc}"))
            .clone()
    };
    let codes = |item: &serde_json::Value| -> Vec<String> {
        item["scope_diagnostics"]
            .as_array()
            .expect("scope_diagnostics array")
            .iter()
            .map(|entry| entry["code"].as_str().expect("code").to_string())
            .collect()
    };

    let healthy_item = find(&healthy);
    assert_eq!(healthy_item["lease_state"], "held", "{healthy_item}");
    // Neither workspace is in the worktree registry (no `worktree add` ran),
    // so both report the missing owner entry — that IS the finding.
    assert!(
        codes(&healthy_item).contains(&"registry_entry_missing".to_string()),
        "{healthy_item}"
    );
    assert!(
        !codes(&healthy_item).contains(&"lease_expired".to_string()),
        "a live lease must not be reported as expired: {healthy_item}"
    );

    let broken_item = find(&broken);
    assert_eq!(broken_item["lease_state"], "expired", "{broken_item}");
    let broken_codes = codes(&broken_item);
    for expected in [
        "lease_expired",
        "workspace_path_missing",
        "registry_entry_missing",
    ] {
        assert!(
            broken_codes.contains(&expected.to_string()),
            "missing {expected} in {broken_item}"
        );
    }
    // Severity is part of the machine contract, not decoration.
    for entry in broken_item["scope_diagnostics"]
        .as_array()
        .expect("scope_diagnostics")
    {
        let severity = entry["severity"].as_str().expect("severity");
        assert!(
            severity == "warning" || severity == "error",
            "unexpected severity {severity}: {broken_item}"
        );
    }
}

/// The W0 read-only skeleton contract, stated on its own: neither the human
/// nor the JSON form of the default invocation may mutate anything.
#[tokio::test]
#[serial]
async fn worktree_doctor_default_invocation_is_readonly() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let workspace_id = seed_linked_workspace(repo.path(), "scope", now_ms()).await;

    let before = repo_snapshot(repo.path()).await;
    for args in [
        vec!["worktree", "doctor"],
        vec!["--json", "worktree", "doctor"],
        vec!["worktree", "doctor", workspace_id.as_str()],
    ] {
        let output = run_libra_command(&args, repo.path());
        assert_cli_success(&output, "read-only doctor invocation");
        let after = repo_snapshot(repo.path()).await;
        assert_eq!(before, after, "`libra {}` wrote state", args.join(" "));
    }
}

/// Envelope, required fields, ordering, page limits, opaque cursor and both
/// fail-closed refusals — the frozen machine contract (§C.8, Codex R18/R19).
#[tokio::test]
#[serial]
async fn worktree_doctor_json_schema_and_pagination_stable() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let conn = repo_db(repo.path()).await;
    let repo_id = repo_identity(&conn).await;
    bulk_seed(&conn, &repo_id, 600).await;

    // Default page size is 50; the sort key is `workspace_id` ascending.
    let default_page = run_libra_command(&["--json", "worktree", "doctor"], repo.path());
    assert_cli_success(&default_page, "default doctor page");
    let doc = parse_json(&default_page.stdout, "default page");
    assert_eq!(doc["command"], "worktree.doctor", "{doc}");
    let items = doc["data"]["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(items.len(), 50, "default limit is 50: {doc}");
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["workspace_id"].as_str().expect("id"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "diagnostics are ordered by workspace_id");
    assert_eq!(ids[0], "bulk-0000", "{doc}");

    // The required field set is frozen schema v1.
    let mut keys: Vec<String> = items[0]
        .as_object()
        .expect("diagnostic object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "kind",
            "lease_expires_at",
            "lease_fence",
            "lease_owner",
            "lease_state",
            "path",
            "repo_id",
            "scope_diagnostics",
            "state",
            "workspace_id",
            "worktree_id",
        ],
        "diagnostic key set is frozen schema v1: {doc}"
    );

    // An over-large --limit is capped at 500, not honoured.
    let capped = run_libra_command(
        &["--json", "worktree", "doctor", "--limit", "5000"],
        repo.path(),
    );
    assert_cli_success(&capped, "capped doctor page");
    let capped_doc = parse_json(&capped.stdout, "capped page");
    assert_eq!(
        capped_doc["data"]["diagnostics"]
            .as_array()
            .expect("capped diagnostics")
            .len(),
        500,
        "page cap is 500"
    );
    assert!(
        capped_doc["data"]["next_cursor"].is_string(),
        "600 rows must leave a next page: {capped_doc}"
    );

    // Keyset walk: the cursor is opaque, and the next page continues strictly
    // after the previous one with no repeats.
    let page1 = run_libra_command(
        &["--json", "worktree", "doctor", "--limit", "2"],
        repo.path(),
    );
    assert_cli_success(&page1, "doctor page 1");
    let doc1 = parse_json(&page1.stdout, "page1");
    let cursor = doc1["data"]["next_cursor"]
        .as_str()
        .expect("cursor")
        .to_string();
    assert_ne!(
        cursor, "bulk-0001",
        "the cursor must be opaque, not a raw workspace_id"
    );
    let page2 = run_libra_command(
        &[
            "--json", "worktree", "doctor", "--limit", "2", "--cursor", &cursor,
        ],
        repo.path(),
    );
    assert_cli_success(&page2, "doctor page 2");
    let doc2 = parse_json(&page2.stdout, "page2");
    let ids2: Vec<&str> = doc2["data"]["diagnostics"]
        .as_array()
        .expect("page2 diagnostics")
        .iter()
        .map(|item| item["workspace_id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids2, vec!["bulk-0002", "bulk-0003"], "{doc2}");

    // Single-scope view: singular key, and NO pagination field.
    let single = run_libra_command(&["--json", "worktree", "doctor", "bulk-0007"], repo.path());
    assert_cli_success(&single, "single scope view");
    let single_doc = parse_json(&single.stdout, "single scope");
    assert_eq!(single_doc["command"], "worktree.doctor", "{single_doc}");
    assert_eq!(single_doc["data"]["schema_version"], 1, "{single_doc}");
    let mut single_keys: Vec<&str> = single_doc["data"]
        .as_object()
        .expect("single-scope data object")
        .keys()
        .map(String::as_str)
        .collect();
    single_keys.sort_unstable();
    assert_eq!(
        single_keys,
        vec!["diagnostic", "schema_version"],
        "the single-scope response has its own exact, non-paginated shape: {single_doc}"
    );
    assert_eq!(
        single_doc["data"]["diagnostic"]["workspace_id"], "bulk-0007",
        "{single_doc}"
    );
    assert!(
        single_doc["data"].get("next_cursor").is_none(),
        "the single-scope view has no pagination keys: {single_doc}"
    );
    assert!(
        single_doc["data"].get("diagnostics").is_none(),
        "the single-scope view uses the singular key: {single_doc}"
    );

    // A cursor this command did not issue fails closed (LBR-WORKTREE-001)
    // instead of quietly restarting at page one.
    let bad_cursor = run_libra_command(
        &["--json", "worktree", "doctor", "--cursor", "not-a-cursor"],
        repo.path(),
    );
    assert!(!bad_cursor.status.success(), "invalid cursor must fail");
    let bad_doc = parse_json(&bad_cursor.stderr, "invalid cursor error");
    assert_eq!(bad_doc["ok"], false, "{bad_doc}");
    assert_eq!(bad_doc["error_code"], "LBR-WORKTREE-001", "{bad_doc}");
    assert_eq!(bad_doc["category"], "repo", "{bad_doc}");
    assert_eq!(bad_doc["exit_code"], 128, "{bad_doc}");

    // A single scope is not a page: combining the id with --limit/--cursor is
    // a usage error, not a silently-ignored flag.
    for extra in [["--limit", "2"], ["--cursor", cursor.as_str()]] {
        let usage = run_libra_command(
            &[
                "--json",
                "worktree",
                "doctor",
                "bulk-0007",
                extra[0],
                extra[1],
            ],
            repo.path(),
        );
        assert_eq!(usage.status.code(), Some(129), "usage exit for {extra:?}");
        let usage_doc = parse_json(&usage.stderr, "usage error");
        assert_eq!(usage_doc["error_code"], "LBR-CLI-002", "{usage_doc}");
        assert_eq!(usage_doc["category"], "cli", "{usage_doc}");
    }
}

/// An unreadable scope is refused (`LBR-WORKTREE-002`) — the doctor never
/// answers with an empty or partial diagnosis built on unknown ownership.
#[tokio::test]
#[serial]
async fn worktree_doctor_corrupt_scope_fails_closed() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    seed_linked_workspace(repo.path(), "scope", now_ms()).await;

    fs::write(repo.path().join(".libra/worktrees.json"), b"{ not json")
        .expect("corrupt the worktree registry");

    let output = run_libra_command(&["--json", "worktree", "doctor"], repo.path());
    assert!(!output.status.success(), "corrupt scope must fail closed");
    let doc = parse_json(&output.stderr, "corrupt scope error");
    assert_eq!(doc["ok"], false, "{doc}");
    assert_eq!(doc["error_code"], "LBR-WORKTREE-002", "{doc}");
    assert_eq!(doc["category"], "repo", "{doc}");
}

/// W4's only adoption path is an explicit, confirmed attribution to a live
/// workspace lease. It atomically converts every legacy row for the provider
/// session and records the irreversible decision in the append-only audit.
#[tokio::test]
#[serial]
async fn worktree_doctor_adopts_legacy_capture_with_confirmation_and_audit() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let workspace_id = seed_linked_workspace(repo.path(), "capture-scope", now_ms()).await;
    let conn = repo_db(repo.path()).await;
    let repo_id = repo_identity(&conn).await;
    let workspace = WorkspaceStore::get_with_conn(&conn, &workspace_id)
        .await
        .expect("read captured workspace")
        .expect("seeded workspace exists");

    for statement in [
        Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at,
                stopped_at, schema_version
             ) VALUES ('legacy-session', 'opencode', 'provider-session', 'active', '.',
                       '{}', '{}', 1, 1, NULL, 1)",
            [],
        ),
        Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_export_job (
                job_id, agent_kind, provider_session_id, observed_generation,
                processed_generation, state, created_at, updated_at, ttl_expires_at
             ) VALUES ('legacy-export', 'opencode', 'provider-session', 1, 0,
                       'dirty', 1, 1, 9999999999999)",
            [],
        ),
        Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, next_ordinal, state, created_at, updated_at
             ) VALUES ('legacy-import', 'opencode', 'provider-session', 'file', 'source',
                       1, 0, 'discovered', 1, 1)",
            [],
        ),
    ] {
        conn.execute_raw(statement)
            .await
            .expect("seed legacy capture row");
    }

    let unconfirmed = run_libra_command(
        &[
            "--json",
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "legacy-session",
        ],
        repo.path(),
    );
    assert_eq!(unconfirmed.status.code(), Some(129), "{unconfirmed:?}");
    let untouched = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT scope_state FROM agent_session WHERE session_id = 'legacy-session'",
        ))
        .await
        .expect("read legacy scope")
        .expect("legacy row");
    assert_eq!(
        untouched
            .try_get_by_index::<String>(0)
            .expect("scope state"),
        "legacy_unknown"
    );

    let adopted = run_libra_command(
        &[
            "--json",
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "legacy-session",
            "--confirm",
        ],
        repo.path(),
    );
    assert_cli_success(&adopted, "confirmed capture-scope adoption");
    let response = parse_json(&adopted.stdout, "capture-scope adoption");
    assert_eq!(
        response["command"], "worktree.doctor.adopt_capture",
        "{response}"
    );
    assert_eq!(response["data"]["workspace_id"], workspace_id, "{response}");
    assert_eq!(response["data"]["repo_id"], repo_id, "{response}");

    for table in ["agent_session", "agent_export_job", "agent_import_identity"] {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                format!(
                    "SELECT scope_state, repo_id, worktree_id, workspace_id, workspace_fence \
                     FROM {table} WHERE provider_session_id = 'provider-session'"
                ),
                [],
            ))
            .await
            .expect("read adopted scope")
            .expect("adopted row");
        assert_eq!(
            row.try_get_by::<String, _>("scope_state")
                .expect("scope state"),
            "scoped"
        );
        assert_eq!(
            row.try_get_by::<String, _>("repo_id").expect("repo id"),
            repo_id
        );
        assert_eq!(
            row.try_get_by::<String, _>("worktree_id")
                .expect("worktree id"),
            workspace.worktree_id.clone().expect("linked worktree id")
        );
        assert_eq!(
            row.try_get_by::<String, _>("workspace_id")
                .expect("workspace id"),
            workspace_id
        );
        assert_eq!(
            row.try_get_by::<i64, _>("workspace_fence")
                .expect("workspace fence"),
            workspace.lease_fence
        );
    }
    let audit_count = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) FROM agent_workspace_scope_audit \
             WHERE action = 'adopt_legacy_capture_scope' AND provider_session_id = 'provider-session'",
        ))
        .await
        .expect("read capture-scope audit")
        .expect("audit count");
    assert_eq!(
        audit_count
            .try_get_by_index::<i64>(0)
            .expect("audit count value"),
        1
    );

    let repeat = run_libra_command(
        &[
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "legacy-session",
            "--confirm",
        ],
        repo.path(),
    );
    assert!(
        !repeat.status.success(),
        "a scoped row cannot be adopted twice"
    );
}

/// An expired lease may still occupy its identity until explicit reclaim, but
/// it is not authority to attribute capture history. Adoption must wait for a
/// renewed or newly fenced live lease and leave the legacy row untouched.
#[tokio::test]
#[serial]
async fn worktree_doctor_refuses_capture_adoption_to_an_expired_lease() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let workspace_id = seed_linked_workspace(repo.path(), "expired-capture", 1_000).await;
    let conn = repo_db(repo.path()).await;
    conn.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            stopped_at, schema_version
         ) VALUES ('expired-legacy-session', 'opencode', 'expired-provider', 'active', '.',
                   '{}', '{}', 1, 1, NULL, 1)"
            .to_string(),
    ))
    .await
    .expect("seed legacy capture row");

    let refused = run_libra_command(
        &[
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "expired-legacy-session",
            "--confirm",
        ],
        repo.path(),
    );
    assert!(!refused.status.success(), "expired lease must be refused");
    let row = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT scope_state FROM agent_session WHERE session_id = 'expired-legacy-session'"
                .to_string(),
        ))
        .await
        .expect("read untouched legacy row")
        .expect("legacy row");
    assert_eq!(
        row.try_get_by_index::<String>(0).expect("scope state"),
        "legacy_unknown"
    );
    let audit = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) FROM agent_workspace_scope_audit \
             WHERE provider_session_id = 'expired-provider'"
                .to_string(),
        ))
        .await
        .expect("read audit count")
        .expect("audit count");
    assert_eq!(audit.try_get_by_index::<i64>(0).expect("audit count"), 0);
}

/// An export or import identity can outlive its parent catalog session. The
/// explicit adoption path must still make that legacy state recoverable rather
/// than leaving it permanently blocked behind a missing `agent_session` row.
#[tokio::test]
#[serial]
async fn worktree_doctor_adopts_orphan_legacy_provider_session() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let workspace_id = seed_linked_workspace(repo.path(), "orphan-capture", now_ms()).await;
    let conn = repo_db(repo.path()).await;
    let repo_id = repo_identity(&conn).await;
    conn.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, next_ordinal, state, created_at, updated_at
         ) VALUES ('orphan-import', 'opencode', 'orphan-provider', 'file', 'source',
                   1, 0, 'discovered', 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed orphan legacy import identity");

    let diagnosis = run_libra_command(&["worktree", "doctor"], repo.path());
    assert_cli_success(&diagnosis, "diagnose orphan legacy capture state");
    assert!(
        String::from_utf8_lossy(&diagnosis.stdout).contains("legacy capture scope"),
        "orphan rows must remain discoverable to the read-only doctor: {diagnosis:?}"
    );

    let adopted = run_libra_command(
        &[
            "--json",
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "orphan-provider",
            "--confirm",
        ],
        repo.path(),
    );
    assert_cli_success(&adopted, "adopt orphan legacy provider session");
    let row = conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT scope_state, repo_id, workspace_id FROM agent_import_identity \
             WHERE identity_id = 'orphan-import'"
                .to_string(),
        ))
        .await
        .expect("read adopted orphan row")
        .expect("adopted orphan row");
    assert_eq!(
        row.try_get_by::<String, _>("scope_state")
            .expect("scope state"),
        "scoped"
    );
    assert_eq!(
        row.try_get_by::<String, _>("repo_id").expect("repo id"),
        repo_id
    );
    assert_eq!(
        row.try_get_by::<String, _>("workspace_id")
            .expect("workspace id"),
        workspace_id
    );
}

/// A catalog session id is the primary doctor lookup key. It must not be
/// interpreted as an unrelated provider id merely because another legacy row
/// happens to use the same opaque string.
#[tokio::test]
#[serial]
async fn worktree_doctor_prefers_catalog_session_id_over_provider_id_collision() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let workspace_id = seed_linked_workspace(repo.path(), "collision-capture", now_ms()).await;
    let conn = repo_db(repo.path()).await;
    conn.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            stopped_at, schema_version
         ) VALUES ('collision-id', 'opencode', 'catalog-provider', 'active', '.',
                   '{}', '{}', 1, 1, NULL, 1)"
            .to_string(),
    ))
    .await
    .expect("seed catalog legacy session");
    conn.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, next_ordinal, state, created_at, updated_at
         ) VALUES ('collision-orphan', 'codex', 'collision-id', 'file', 'source',
                   1, 0, 'discovered', 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed colliding orphan provider id");

    let adopted = run_libra_command(
        &[
            "worktree",
            "doctor",
            &workspace_id,
            "--adopt-capture-session",
            "collision-id",
            "--confirm",
        ],
        repo.path(),
    );
    assert_cli_success(&adopted, "adopt exact catalog session id");

    for (table, predicate, expected_scope) in [
        ("agent_session", "session_id = 'collision-id'", "scoped"),
        (
            "agent_import_identity",
            "identity_id = 'collision-orphan'",
            "legacy_unknown",
        ),
    ] {
        let row = conn
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT scope_state FROM {table} WHERE {predicate}"),
            ))
            .await
            .expect("read collision scope")
            .expect("collision row");
        assert_eq!(
            row.try_get_by_index::<String>(0).expect("scope state"),
            expected_scope,
            "{table} must retain its independently selected scope"
        );
    }
}

/// The bare `worktree doctor` hint stays inspect-only even though W4 adds one
/// separately named, confirmed adoption grammar. A normal diagnostic must not
/// promise recovery as a side effect.
#[test]
fn worktree_doctor_hints_are_inspect_only() {
    const SOURCES: [(&str, &str); 3] = [
        (
            "workspace.rs",
            include_str!("../../src/internal/workspace.rs"),
        ),
        ("worktree.rs", include_str!("../../src/command/worktree.rs")),
        (
            "environment.rs",
            include_str!("../../src/internal/ai/runtime/environment.rs"),
        ),
    ];
    for (name, source) in SOURCES {
        for (index, line) in source.lines().enumerate() {
            if !line.contains("worktree doctor") {
                continue;
            }
            // Comments explain the rule itself; only user-visible strings and
            // the surrounding sentence of a hint are constrained.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // Separately named, `--confirm`-gated action grammars are the
            // sanctioned exception: naming one is not the bare diagnostic
            // promising recovery, it is pointing at the explicit command
            // that performs it.
            if ["--adopt-capture-session", "--adopt-info-to"]
                .iter()
                .any(|action| line.contains(action))
            {
                continue;
            }
            for forbidden in ["reclaim", "adopt", "release"] {
                assert!(
                    !line.contains(forbidden),
                    "{name}:{} promises a `{forbidden}` action the bare `worktree doctor` \
                     cannot perform: {line}",
                    index + 1
                );
            }
        }
    }
}

/// Every published recovery instruction must include the confirmation the
/// mutating command now enforces. Otherwise a diagnostic sends an operator
/// into a guaranteed refusal precisely when the repository needs repair.
#[test]
fn worktree_repair_guidance_requires_confirmation() {
    for (name, document) in [
        (
            "English worktree docs",
            include_str!("../../docs/commands/worktree.md"),
        ),
        (
            "Chinese worktree docs",
            include_str!("../../docs/commands/zh-CN/worktree.md"),
        ),
    ] {
        assert!(
            document.contains("worktree repair --confirm"),
            "{name} must document the required confirmation"
        );
        assert!(
            !document.contains("`libra worktree repair`"),
            "{name} still suggests an unconfirmed repair command"
        );
        assert!(
            !document.contains("`worktree repair --migrate-layout`"),
            "{name} still suggests an unconfirmed layout migration"
        );
    }

    let source = include_str!("../../src/command/worktree.rs");
    for stale_instruction in [
        "`libra worktree repair` to heal it",
        "`libra worktree repair` first",
        "`libra worktree repair` retries it",
        "`worktree repair --migrate-layout` from the MAIN worktree",
        "rerun `worktree repair`",
    ] {
        assert!(
            !source.contains(stale_instruction),
            "worktree diagnostic still recommends a command that will be refused: {stale_instruction}"
        );
    }
}

/// The only mutating doctor grammar must be discoverable in both user-facing
/// command references. Otherwise legacy rows are deliberately fail-closed but
/// operators have no documented recovery route.
#[test]
fn worktree_doctor_capture_adoption_is_documented_in_both_languages() {
    for (name, document) in [
        (
            "English worktree docs",
            include_str!("../../docs/commands/worktree.md"),
        ),
        (
            "Chinese worktree docs",
            include_str!("../../docs/commands/zh-CN/worktree.md"),
        ),
    ] {
        assert!(
            document.contains("--adopt-capture-session <session-id> --confirm"),
            "{name} must document the confirmed legacy capture adoption grammar"
        );
        assert!(
            document.contains("worktree.doctor.adopt_capture"),
            "{name} must document the adoption JSON envelope"
        );
    }
}

// ---------------------------------------------------------------------------
// W0 (§C.11, Codex R16/R17): mutating repair actions require `--confirm` and
// emit exactly one operation-log audit event per executed action.
// ---------------------------------------------------------------------------

/// One worktree directory's full content tree (relative path -> bytes), with
/// a symlink recorded as a `path -> target` entry so a legacy `.libra` link
/// is compared as a link, never followed.
#[cfg(unix)]
fn worktree_dir_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("walked path is beneath its root")
                .to_string_lossy()
                .to_string();
            let file_type = entry.file_type().expect("file type");
            if file_type.is_symlink() {
                let target = fs::read_link(&path).expect("read symlink target");
                entries.push((format!("{relative} -> {}", target.display()), Vec::new()));
            } else if file_type.is_dir() {
                stack.push(path);
            } else {
                entries.push((relative, fs::read(&path).expect("read file")));
            }
        }
    }
    entries.sort();
    entries
}

/// The repository's operation rows, newest first, read through the public
/// operation service (the same API `libra op log` paginates).
#[cfg(unix)]
async fn operation_rows(repo: &Path) -> Vec<libra::internal::operation::OperationLogListItem> {
    use libra::internal::operation::{OperationQueryPage, OperationService};

    let conn = repo_db(repo).await;
    let repo_id = repo_identity(&conn).await;
    let page = OperationService::list_operations_by_repo_paginated_with_conn(
        &conn,
        &repo_id,
        OperationQueryPage {
            page: 1,
            per_page: 200,
        },
    )
    .await
    .expect("list operation rows");
    let mut rows = page.items;
    rows.sort_by(|a, b| a.op_id.cmp(&b.op_id));
    rows
}

/// W0 (§C.11, Codex R16/R17), table-driven over the three mutating repair
/// actions — `repair <path>`, the no-arg registry repair, and a non-dry-run
/// `--migrate-layout`:
///
/// 1. WITHOUT `--confirm` the action is refused with ZERO side effects: the
///    registry file, the operation table and every watched worktree
///    directory are byte-identical before and after.
/// 2. WITH `--confirm` the action runs, exactly ONE new operation row names
///    it (complete: actor, outcome, finish timestamp), and only the action's
///    target scope changes — a bystander worktree is byte-identical.
///
/// Unix-only because the legacy-symlink layout the migration acts on cannot
/// be built without `symlink(2)`.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn worktree_doctor_mutations_require_confirmation_and_emit_audit() {
    use libra::internal::operation::OperationStatus;

    enum RepairAction {
        IdentityPath,
        RegistryAll,
        MigrateLayout,
        DoctorAdoptInfo,
        DoctorClearInfo,
    }

    let table = [
        (RepairAction::IdentityPath, "worktree repair <path>"),
        (RepairAction::RegistryAll, "worktree repair"),
        (
            RepairAction::MigrateLayout,
            "worktree repair --migrate-layout",
        ),
        (
            RepairAction::DoctorAdoptInfo,
            "worktree doctor --adopt-info-to",
        ),
        (
            RepairAction::DoctorClearInfo,
            "worktree doctor --clear-common-info",
        ),
    ];

    for (action, command_name) in table {
        let repo = create_committed_repo_via_cli();
        let main = repo.path();

        // Two scopes in every row: the action's target and a bystander the
        // action must never touch.
        let target = main.join("wt-target");
        let bystander = main.join("wt-bystander");
        assert_cli_success(
            &run_libra_command(&["worktree", "add", "wt-bystander"], main),
            "add bystander worktree",
        );

        // Per-row setup: what the action repairs, and its argv.
        let argv: Vec<String> = match action {
            RepairAction::IdentityPath => {
                assert_cli_success(
                    &run_libra_command(&["worktree", "add", "wt-target"], main),
                    "add target worktree",
                );
                fs::remove_file(target.join(".libra").join("worktree_id"))
                    .expect("damage the target's gitdir identity");
                vec!["worktree".into(), "repair".into(), "wt-target".into()]
            }
            RepairAction::RegistryAll => {
                assert_cli_success(
                    &run_libra_command(&["worktree", "add", "wt-target"], main),
                    "add target worktree",
                );
                // A duplicated registry entry: exactly what the no-arg
                // repair exists to remove.
                let registry = main.join(".libra").join("worktrees.json");
                let mut doc: serde_json::Value =
                    serde_json::from_slice(&fs::read(&registry).expect("read registry"))
                        .expect("registry json");
                let duplicate = doc["entries"]
                    .as_array()
                    .expect("entries")
                    .iter()
                    .find(|entry| {
                        entry["path"]
                            .as_str()
                            .is_some_and(|path| path.ends_with("wt-target"))
                    })
                    .expect("target entry")
                    .clone();
                doc["entries"]
                    .as_array_mut()
                    .expect("entries")
                    .push(duplicate);
                fs::write(
                    &registry,
                    serde_json::to_vec_pretty(&doc).expect("serialize"),
                )
                .expect("plant the duplicate entry");
                vec!["worktree".into(), "repair".into()]
            }
            RepairAction::MigrateLayout => {
                // The target is a LEGACY shared-.libra symlink worktree;
                // migration replaces its gitdir with a real one.
                fs::create_dir_all(&target).expect("mkdir legacy target");
                std::os::unix::fs::symlink(main.join(".libra"), target.join(".libra"))
                    .expect("legacy symlink");
                // Materialize the registry, then register the legacy entry
                // the way the v1→v2 upgrade would have backfilled it.
                assert_cli_success(
                    &run_libra_command(&["worktree", "repair", "--confirm"], main),
                    "materialize registry",
                );
                let registry = main.join(".libra").join("worktrees.json");
                let mut doc: serde_json::Value =
                    serde_json::from_slice(&fs::read(&registry).expect("read registry"))
                        .expect("registry json");
                let canonical = target.canonicalize().expect("canonical target");
                doc["entries"]
                    .as_array_mut()
                    .expect("entries")
                    .push(serde_json::json!({
                        "path": canonical.to_string_lossy(),
                        "is_main": false,
                        "locked": false,
                        "lock_reason": null,
                        "worktree_id": "legacy-wt-target",
                    }));
                fs::write(
                    &registry,
                    serde_json::to_vec_pretty(&doc).expect("serialize"),
                )
                .expect("register the legacy entry");
                vec![
                    "worktree".into(),
                    "repair".into(),
                    "--migrate-layout".into(),
                ]
            }
            // W0 §C.4.1.1: the two info-file doctor actions join the same
            // confirmation + single-audit-row contract as the repairs.
            RepairAction::DoctorAdoptInfo => {
                assert_cli_success(
                    &run_libra_command(&["worktree", "add", "wt-target"], main),
                    "add target worktree",
                );
                let info_dir = main.join(".libra").join("info");
                fs::create_dir_all(&info_dir).expect("info dir");
                fs::write(info_dir.join("exclude"), b"legacy.tmp\n").expect("common exclude");
                vec![
                    "worktree".into(),
                    "doctor".into(),
                    "--adopt-info-to".into(),
                    "wt-target".into(),
                ]
            }
            RepairAction::DoctorClearInfo => {
                assert_cli_success(
                    &run_libra_command(&["worktree", "add", "wt-target"], main),
                    "add target worktree",
                );
                let info_dir = main.join(".libra").join("info");
                fs::create_dir_all(&info_dir).expect("info dir");
                fs::write(info_dir.join("exclude"), b"legacy.tmp\n").expect("common exclude");
                fs::write(info_dir.join("attributes"), b"*.dat filter=lfs\n")
                    .expect("common attributes");
                vec![
                    "worktree".into(),
                    "doctor".into(),
                    "--clear-common-info".into(),
                ]
            }
        };

        // ---- Phase 1: no `--confirm` -> refused, ZERO side effects. ----
        // Clear any maintenance lock left by the setup writers so the
        // refusal phase can assert the action did not CREATE one.
        let maintenance_lock = main.join(".libra").join("maintenance.lock");
        match fs::remove_file(&maintenance_lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot clear the maintenance lock: {error}"),
        }
        let registry_path = main.join(".libra").join("worktrees.json");
        let registry_before = fs::read(&registry_path).expect("registry before");
        let operations_before = operation_rows(main).await;
        let target_tree_before = worktree_dir_tree(&target);
        let bystander_tree_before = worktree_dir_tree(&bystander);

        let refused = run_libra_command(&argv.iter().map(String::as_str).collect::<Vec<_>>(), main);
        assert!(
            !refused.status.success(),
            "{command_name} without --confirm must be refused: {}",
            String::from_utf8_lossy(&refused.stdout)
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.contains("without confirmation; re-run with --confirm"),
            "{command_name}: the refusal names the required flag: {stderr}"
        );
        assert_eq!(
            fs::read(&registry_path).expect("registry after refusal"),
            registry_before,
            "{command_name}: the refusal must not touch the registry"
        );
        assert_eq!(
            operation_rows(main).await,
            operations_before,
            "{command_name}: the refusal must not write an operation row"
        );
        assert_eq!(
            worktree_dir_tree(&target),
            target_tree_before,
            "{command_name}: the refusal must not touch the target worktree"
        );
        assert_eq!(
            worktree_dir_tree(&bystander),
            bystander_tree_before,
            "{command_name}: the refusal must not touch the bystander worktree"
        );
        // The refusal path must stay classified read-only end to end: a
        // writer classification would take the shared maintenance hold,
        // whose first side effect is CREATING `.libra/maintenance.lock`
        // (cleared above after setup, so presence here means the refusal
        // created it).
        assert!(
            !maintenance_lock.exists(),
            "{command_name}: an unconfirmed action must not create the \
             maintenance lock (its CommandScope must remain read-only)"
        );

        // ---- Phase 2: `--confirm` -> runs, exactly one audit row, only the
        // target scope modified. ----
        let mut confirmed = argv.clone();
        confirmed.push("--confirm".into());
        let ran = run_libra_command(
            &confirmed.iter().map(String::as_str).collect::<Vec<_>>(),
            main,
        );
        assert_cli_success(&ran, "confirmed repair action");

        let operations_after = operation_rows(main).await;
        assert_eq!(
            operations_after.len(),
            operations_before.len() + 1,
            "{command_name}: exactly one audit row per executed action"
        );
        let new_rows: Vec<_> = operations_after
            .iter()
            .filter(|after| {
                !operations_before
                    .iter()
                    .any(|before| before.op_id == after.op_id)
            })
            .collect();
        assert_eq!(new_rows.len(), 1, "{command_name}: the audit row is unique");
        let audit = new_rows[0];
        assert_eq!(
            audit.command_name, command_name,
            "the audit row names the action"
        );
        assert_eq!(
            audit.status,
            OperationStatus::Succeeded,
            "the audit row records the outcome"
        );
        assert!(
            audit.description.contains(command_name),
            "the audit row describes the action: {}",
            audit.description
        );
        assert!(!audit.actor.is_empty(), "the audit row names an actor");
        assert!(
            audit.end_ts.is_some(),
            "the audit row is finished, not left running"
        );

        // Only the target scope changed.
        assert_eq!(
            worktree_dir_tree(&bystander),
            bystander_tree_before,
            "{command_name}: the bystander worktree is untouched"
        );
        match action {
            RepairAction::IdentityPath => {
                // The target's identity was restored FROM THE REGISTRY, and
                // the registry itself was not rewritten.
                let restored = fs::read_to_string(target.join(".libra").join("worktree_id"))
                    .expect("identity restored");
                let doc: serde_json::Value =
                    serde_json::from_slice(&fs::read(&registry_path).expect("registry"))
                        .expect("registry json");
                let persisted = doc["entries"]
                    .as_array()
                    .expect("entries")
                    .iter()
                    .find(|entry| {
                        entry["path"]
                            .as_str()
                            .is_some_and(|path| path.ends_with("wt-target"))
                    })
                    .and_then(|entry| entry["worktree_id"].as_str())
                    .expect("persisted id");
                assert_eq!(
                    restored.trim(),
                    persisted,
                    "the restored identity is the registry's persisted id"
                );
                assert_eq!(
                    fs::read(&registry_path).expect("registry after repair"),
                    registry_before,
                    "identity repair touches the target gitdir only"
                );
            }
            RepairAction::RegistryAll => {
                // The registry (this action's target scope) was healed: the
                // duplicate is gone and neither worktree directory moved.
                let healed = fs::read(&registry_path).expect("registry after repair");
                assert_ne!(healed, registry_before, "the duplicate was removed");
                let doc: serde_json::Value =
                    serde_json::from_slice(&healed).expect("registry json");
                let target_entries = doc["entries"]
                    .as_array()
                    .expect("entries")
                    .iter()
                    .filter(|entry| {
                        entry["path"]
                            .as_str()
                            .is_some_and(|path| path.ends_with("wt-target"))
                    })
                    .count();
                assert_eq!(target_entries, 1, "the duplicate entry is gone");
                assert_eq!(
                    worktree_dir_tree(&target),
                    target_tree_before,
                    "registry repair touches no worktree directory"
                );
            }
            RepairAction::MigrateLayout => {
                // The legacy target (this action's target scope) now has a
                // REAL gitdir; the registry's SEMANTIC content is unchanged
                // (compared as parsed JSON — the migration's save normalizes
                // key order, which is not a scope mutation).
                assert!(
                    fs::symlink_metadata(target.join(".libra"))
                        .expect("gitdir metadata")
                        .file_type()
                        .is_dir(),
                    "the legacy symlink was replaced by a real gitdir"
                );
                let after: serde_json::Value = serde_json::from_slice(
                    &fs::read(&registry_path).expect("registry after migration"),
                )
                .expect("registry after migration parses");
                let before: serde_json::Value =
                    serde_json::from_slice(&registry_before).expect("registry before parses");
                assert_eq!(
                    after, before,
                    "migration touches the target gitdir only (registry semantics)"
                );
            }
            RepairAction::DoctorAdoptInfo => {
                // The common file was copied into the TARGET's local gitdir;
                // the common original and the registry are untouched.
                assert_eq!(
                    fs::read(target.join(".libra").join("info").join("exclude"))
                        .expect("adopted exclude"),
                    b"legacy.tmp\n",
                    "the common info/exclude was adopted into the target"
                );
                assert_eq!(
                    fs::read(main.join(".libra").join("info").join("exclude"))
                        .expect("common exclude kept"),
                    b"legacy.tmp\n",
                    "adoption copies; it never moves or clears the common file"
                );
                assert_eq!(
                    fs::read(&registry_path).expect("registry after adopt"),
                    registry_before,
                    "info adoption does not touch the registry"
                );
            }
            RepairAction::DoctorClearInfo => {
                // The common files are gone; the target worktree and the
                // registry are untouched.
                assert!(
                    !main.join(".libra").join("info").join("exclude").exists(),
                    "the common info/exclude was cleared"
                );
                assert!(
                    !main.join(".libra").join("info").join("attributes").exists(),
                    "the common info/attributes was cleared"
                );
                assert_eq!(
                    worktree_dir_tree(&target),
                    target_tree_before,
                    "clearing the common files touches no worktree directory"
                );
                assert_eq!(
                    fs::read(&registry_path).expect("registry after clear"),
                    registry_before,
                    "info clearing does not touch the registry"
                );
            }
        }
    }
}
