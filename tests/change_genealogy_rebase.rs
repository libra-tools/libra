//! CH-03 rewrite predecessor contract.

use libra::internal::{
    change::{
        ChangeRevisionBuilder, ChangeStore, PredecessorEdge, RelationKind, evolution_for_commit,
        insert_predecessor,
    },
    db,
};
use tempfile::tempdir;

#[tokio::test]
async fn rebase_edge_is_typed_and_queryable() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    insert_predecessor(
        &database,
        &PredecessorEdge {
            successor_oid: "successor".to_string(),
            predecessor_oid: "predecessor".to_string(),
            op_id: "op-rebase".to_string(),
            relation_kind: RelationKind::Rebase,
            ordinal: 0,
        },
    )
    .await
    .expect("edge");
    let edges = evolution_for_commit(&database, "successor", 10)
        .await
        .expect("query");
    assert_eq!(edges[0].relation_kind, RelationKind::Rebase);
    assert_eq!(edges[0].predecessor_oid, "predecessor");
}

#[tokio::test]
async fn builder_inherits_change_id_and_rolls_back_projection_edges() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let first = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-new")
        .set_commit_oid("base")
        .build()
        .await
        .expect("new change");
    let rewritten =
        ChangeRevisionBuilder::for_rewrite(database.clone(), "repo", "op-rewrite", first.change_id)
            .set_commit_oid("rebased")
            .set_predecessors([(String::from("base"), RelationKind::Rebase)])
            .build()
            .await
            .expect("rewrite");
    assert_eq!(rewritten.change_id, first.change_id);
    assert_eq!(
        ChangeStore::new(database.clone())
            .revisions_for_change("repo", first.change_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        evolution_for_commit(&database, "rebased", 10)
            .await
            .unwrap()[0]
            .relation_kind,
        RelationKind::Rebase
    );

    let failed =
        ChangeRevisionBuilder::for_rewrite(database.clone(), "repo", "op-failing", first.change_id)
            .set_commit_oid("atomic-fail")
            .set_predecessors([
                (String::from("base"), RelationKind::Rebase),
                (String::from("base"), RelationKind::Rebase),
            ])
            .build()
            .await;
    assert!(failed.is_err(), "duplicate edge must fail closed");
    assert!(
        ChangeStore::new(database)
            .revisions_for_change("repo", first.change_id)
            .await
            .unwrap()
            .iter()
            .all(|revision| revision.commit_oid != "atomic-fail")
    );
}
