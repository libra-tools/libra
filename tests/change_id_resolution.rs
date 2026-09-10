//! M5 Change ID projection and prefix resolution contract.

use std::str::FromStr;

use libra::internal::{
    change::{
        ChangeId, ChangeIdResolution, ChangeRevision, ChangeStore, RevisionVisibility,
        resolve_change_id_prefix,
    },
    db,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use tempfile::tempdir;

#[tokio::test]
async fn exact_ambiguous_and_not_found_prefixes_are_explicit() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let store = ChangeStore::new(database.clone());
    let first = ChangeId::from_str("abcdef00000000000000000000000001").unwrap();
    let second = ChangeId::from_str("abcdef00000000000000000000000002").unwrap();
    store
        .insert_identity("repo", first, "generated", "op-1")
        .await
        .expect("first identity");
    store
        .insert_identity("repo", second, "generated", "op-2")
        .await
        .expect("second identity");
    assert_eq!(
        resolve_change_id_prefix(&database, "repo", &first.to_hex())
            .await
            .unwrap(),
        ChangeIdResolution::Exact(first)
    );
    assert_eq!(
        resolve_change_id_prefix(&database, "repo", &first.short_prefix(1))
            .await
            .unwrap(),
        ChangeIdResolution::Ambiguous(vec![first, second])
    );
    assert_eq!(
        resolve_change_id_prefix(&database, "repo", &first.short_prefix(32))
            .await
            .unwrap(),
        ChangeIdResolution::Exact(first)
    );
    assert!(matches!(
        resolve_change_id_prefix(&database, "repo", "abcdef")
            .await
            .unwrap(),
        ChangeIdResolution::Ambiguous(ids) if ids == vec![first, second]
    ));
    assert_eq!(
        resolve_change_id_prefix(&database, "repo", "1234")
            .await
            .unwrap(),
        ChangeIdResolution::NotFound
    );
    store
        .insert_revision(&ChangeRevision {
            change_id: first,
            commit_oid: "commit-1".to_string(),
            created_op_id: "op-1".to_string(),
            visibility: RevisionVisibility::Visible,
            revision_ordinal: 0,
        })
        .await
        .expect("revision");
    assert_eq!(
        store
            .revisions_for_change("repo", first)
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .insert_revision(&ChangeRevision {
            change_id: first,
            commit_oid: "commit-2".to_string(),
            created_op_id: "op-2".to_string(),
            visibility: RevisionVisibility::Hidden,
            revision_ordinal: 1,
        })
        .await
        .expect("hidden revision");
    let revisions = store.revisions_for_change("repo", first).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1].visibility, RevisionVisibility::Hidden);
    assert_eq!(revisions[0].commit_oid, "commit-1");
    assert_eq!(revisions[0].created_op_id, "op-1");
    assert_eq!(revisions[0].revision_ordinal, 0);
    assert_eq!(revisions[1].commit_oid, "commit-2");
    assert_eq!(revisions[1].created_op_id, "op-2");
    assert_eq!(revisions[1].revision_ordinal, 1);
    assert_eq!(
        serde_json::to_string(&ChangeIdResolution::Exact(first)).unwrap(),
        format!(r#"{{"Exact":"{}"}}"#, first)
    );
    store
        .ensure_identity("repo", first, "generated", "op-1")
        .await
        .expect("same-repo identity is idempotent");
    assert!(matches!(
        store
            .ensure_identity("other-repo", first, "generated", "op-x")
            .await,
        Err(libra::internal::change::ChangeStoreError::Collision(_))
    ));
    let plan = database
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "EXPLAIN QUERY PLAN SELECT change_id FROM change_identity WHERE repo_id = 'repo' AND change_id GLOB 'abcdef*' ORDER BY change_id",
        ))
        .await
        .expect("indexed prefix query");
    let plan = plan
        .iter()
        .map(|row| row.try_get_by::<String, _>("detail").unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan.contains("idx_change_identity_v2_repo_change"),
        "prefix resolver must use its covering index, got:\n{plan}"
    );
    assert!(
        resolve_change_id_prefix(&database, "repo", "")
            .await
            .is_err()
    );
    assert!(
        resolve_change_id_prefix(&database, "repo", &"a".repeat(33))
            .await
            .is_err()
    );
    assert!(
        resolve_change_id_prefix(&database, "repo", "zzzz")
            .await
            .is_err()
    );
}
