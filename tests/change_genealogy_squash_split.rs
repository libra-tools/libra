//! CH-04 multi-edge genealogy contract.

use libra::internal::change::{
    AiOperationLink, ChangeRevisionBuilder, PredecessorEdge, RelationKind, ai_links_for_change,
    ai_links_for_intent, attach_pending_ai_operation_links, evolution_for_commit,
    insert_predecessor, link_ai_operation, record_pending_ai_operation_link,
};
use libra::internal::db;
use tempfile::tempdir;

#[tokio::test]
async fn squash_and_split_edges_keep_order_and_relation_kind() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    for (ordinal, predecessor, relation) in [
        (0, "left", RelationKind::Squash),
        (1, "right", RelationKind::Squash),
        (2, "split-child", RelationKind::Split),
    ] {
        insert_predecessor(
            &database,
            &PredecessorEdge {
                successor_oid: "successor".to_string(),
                predecessor_oid: predecessor.to_string(),
                op_id: "op-genealogy".to_string(),
                relation_kind: relation,
                ordinal,
            },
        )
        .await
        .expect("edge");
    }
    let edges = evolution_for_commit(&database, "successor", 10)
        .await
        .expect("query");
    assert_eq!(edges.len(), 3);
    assert_eq!(edges[0].relation_kind, RelationKind::Squash);
    assert_eq!(edges[1].relation_kind, RelationKind::Squash);
    assert_eq!(edges[2].relation_kind, RelationKind::Split);
}

#[tokio::test]
async fn builder_records_squash_split_and_duplicate_with_distinct_visible_changes() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");

    let squash = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-squash")
        .set_commit_oid("squash-commit")
        .set_predecessors([
            ("left-parent", RelationKind::Squash),
            ("right-parent", RelationKind::Squash),
        ])
        .build()
        .await
        .expect("squash revision");
    let squash_edges = evolution_for_commit(&database, "squash-commit", 10)
        .await
        .expect("squash edges");
    assert_eq!(squash_edges.len(), 2);
    assert!(
        squash_edges
            .iter()
            .all(|edge| edge.relation_kind == RelationKind::Squash)
    );

    let split_left = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-split")
        .set_commit_oid("split-left")
        .set_predecessors([(String::from("squash-commit"), RelationKind::Split)])
        .build()
        .await
        .expect("left split revision");
    let split_right = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-split")
        .set_commit_oid("split-right")
        .set_predecessors([(String::from("squash-commit"), RelationKind::Split)])
        .build()
        .await
        .expect("right split revision");
    assert_ne!(split_left.change_id, split_right.change_id);
    assert_eq!(
        evolution_for_commit(&database, "split-left", 10)
            .await
            .unwrap()[0]
            .relation_kind,
        RelationKind::Split
    );

    let duplicate = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-duplicate")
        .set_commit_oid("duplicate-commit")
        .set_predecessors([(String::from("squash-commit"), RelationKind::Duplicate)])
        .build()
        .await
        .expect("duplicate revision");
    assert_ne!(duplicate.change_id, squash.change_id);
    assert_eq!(
        evolution_for_commit(&database, "duplicate-commit", 10)
            .await
            .unwrap()[0]
            .relation_kind,
        RelationKind::Duplicate
    );
}

#[tokio::test]
async fn ai_links_query_by_intent_returns_stable_change_id() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let revision = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-change")
        .set_commit_oid("commit-oid-never-used-as-link-key")
        .build()
        .await
        .expect("revision");

    link_ai_operation(
        &database,
        &AiOperationLink {
            operation_id: "operation-redacted".to_string(),
            change_id: revision.change_id,
            session_id: Some("session-redacted".to_string()),
            run_id: Some("run-redacted".to_string()),
            tool_invocation_id: Some("invocation-redacted".to_string()),
            intent_id: Some("intent-redacted".to_string()),
            repo_id: "repo".to_string(),
            worktree_id: None,
            workspace_id: None,
            lease_generation: Some(7),
            config_provenance_digest: Some("digest-redacted".to_string()),
            redaction_version: "v1".to_string(),
        },
    )
    .await
    .expect("AI link");

    let by_intent = ai_links_for_intent(&database, "repo", "intent-redacted")
        .await
        .expect("intent query");
    assert_eq!(by_intent.len(), 1);
    assert_eq!(by_intent[0].change_id, revision.change_id);
    assert_eq!(by_intent[0].operation_id, "operation-redacted");
    let by_change = ai_links_for_change(&database, "repo", revision.change_id)
        .await
        .expect("change query");
    assert_eq!(by_change, by_intent);

    record_pending_ai_operation_link(
        &database,
        "tool-op-1",
        Some("session-1"),
        Some("run-1"),
        Some("tool-call-1"),
        Some("intent-redacted"),
        "repo",
        "v1",
    )
    .await
    .expect("pending AI link");
    attach_pending_ai_operation_links(
        &database,
        "repo",
        revision.change_id,
        Some("tool-op-1"),
        Some("run-1"),
        Some("intent-redacted"),
    )
    .await
    .expect("attach pending AI link");
    let by_intent = ai_links_for_intent(&database, "repo", "intent-redacted")
        .await
        .expect("intent query after attach");
    assert_eq!(by_intent.len(), 2);
    assert_eq!(by_intent[1].operation_id, "tool-op-1");
    assert_eq!(
        by_intent[1].tool_invocation_id.as_deref(),
        Some("tool-call-1")
    );
}
