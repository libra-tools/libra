//! Part C W4 machine interface: `libra agent workspace list|show`
//! (read-only over `workspace_record`, keyset pagination §C.14).

use std::path::Path;

use libra::internal::workspace::{AcquireRequest, WorkspaceStore};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

use super::*;

async fn repo_db(repo: &Path) -> DatabaseConnection {
    let url = format!("sqlite://{}", repo.join(".libra/libra.db").display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    Database::connect(opts).await.expect("open repo db")
}

/// Acquire `count` linked-workspace records (worktree dirs materialized
/// on disk, as the store requires) and return their ids sorted.
async fn seed_workspaces(repo: &Path, count: usize) -> Vec<String> {
    let conn = repo_db(repo).await;
    let mut ids = Vec::new();
    for index in 0..count {
        let dir = repo.join(format!("ws-{index}"));
        fs::create_dir_all(&dir).expect("create workspace dir");
        let request = AcquireRequest::linked(
            format!("wt-{index}"),
            dir.canonicalize().expect("canonical workspace dir"),
            format!("agent-{index}"),
            60_000,
        );
        let lease = WorkspaceStore::acquire(&conn, &request, 1_000 + index as i64)
            .await
            .expect("acquire linked workspace");
        ids.push(lease.workspace_id.clone());
    }
    ids.sort();
    ids
}

/// Keyset pagination walks every record exactly once; the state filter
/// and per-record shape are stable schema v1.
#[tokio::test]
#[serial]
async fn agent_workspace_list_paginates_and_shows() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let ids = seed_workspaces(repo.path(), 3).await;

    // Page 1 (limit 2) + page 2 via the cursor: no dup, no loss.
    let page1 = run_libra_command(
        &["--json", "agent", "workspace", "list", "--limit", "2"],
        repo.path(),
    );
    assert_cli_success(&page1, "workspace list page 1");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&page1.stdout)).expect("page1 json");
    assert_eq!(doc["data"]["schema_version"], 1, "{doc}");
    let items1 = doc["data"]["workspaces"].as_array().expect("items");
    assert_eq!(items1.len(), 2, "{doc}");
    let cursor = doc["data"]["next_cursor"]
        .as_str()
        .expect("cursor")
        .to_string();

    let page2 = run_libra_command(
        &[
            "--json",
            "agent",
            "workspace",
            "list",
            "--limit",
            "2",
            "--cursor",
            &cursor,
        ],
        repo.path(),
    );
    assert_cli_success(&page2, "workspace list page 2");
    let doc2: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&page2.stdout)).expect("page2 json");
    let items2 = doc2["data"]["workspaces"].as_array().expect("items2");
    assert_eq!(items2.len(), 1, "{doc2}");
    assert_eq!(
        doc2["data"]["next_cursor"],
        serde_json::Value::Null,
        "{doc2}"
    );
    let mut walked: Vec<String> = items1
        .iter()
        .chain(items2.iter())
        .map(|item| item["workspace_id"].as_str().expect("id").to_string())
        .collect();
    walked.sort();
    assert_eq!(walked, ids, "keyset walk covers every record once");

    // State filter: everything seeded is active; released filter is empty.
    let active = run_libra_command(
        &["--json", "agent", "workspace", "list", "--state", "active"],
        repo.path(),
    );
    assert_cli_success(&active, "state filter active");
    let doc3: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&active.stdout)).expect("filter json");
    assert_eq!(
        doc3["data"]["workspaces"]
            .as_array()
            .expect("filtered")
            .len(),
        3,
        "{doc3}"
    );
    let released = run_libra_command(
        &[
            "--json",
            "agent",
            "workspace",
            "list",
            "--state",
            "released",
        ],
        repo.path(),
    );
    assert_cli_success(&released, "state filter released");
    let doc4: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&released.stdout)).expect("filter json");
    assert_eq!(
        doc4["data"]["workspaces"]
            .as_array()
            .expect("filtered")
            .len(),
        0,
        "{doc4}"
    );

    // Show: the full stable record shape for one id.
    let show = run_libra_command(
        &["--json", "agent", "workspace", "show", &ids[0]],
        repo.path(),
    );
    assert_cli_success(&show, "workspace show");
    let doc5: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&show.stdout)).expect("show json");
    let record = &doc5["data"]["workspace"];
    assert_eq!(record["workspace_id"], ids[0], "{doc5}");
    assert_eq!(record["kind"], "linked", "{doc5}");
    assert_eq!(record["state"], "active", "{doc5}");
    assert!(record["lease_owner"].is_string(), "{doc5}");
    let mut keys: Vec<_> = record
        .as_object()
        .expect("record object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "base_commit",
            "branch",
            "created_at",
            "kind",
            "lease_expires_at",
            "lease_fence",
            "lease_owner",
            "owner_id",
            "owner_kind",
            "path",
            "session_id",
            "state",
            "task_id",
            "updated_at",
            "workspace_id",
            "worktree_id",
        ],
        "record key set is frozen schema v1: {doc5}"
    );

    // Unknown id and invalid state fail closed with actionable errors.
    let missing = run_libra_command(&["agent", "workspace", "show", "nope"], repo.path());
    assert!(!missing.status.success(), "unknown id fails");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no workspace matches id 'nope'"),
        "actionable unknown-id error"
    );
    let bad_state = run_libra_command(
        &["agent", "workspace", "list", "--state", "bogus"],
        repo.path(),
    );
    assert!(!bad_state.status.success(), "invalid state fails");
    assert!(
        String::from_utf8_lossy(&bad_state.stderr).contains("invalid --state 'bogus'"),
        "actionable state error"
    );
}
