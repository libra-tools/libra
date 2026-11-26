//! Invalid authoritative HEAD state must never become a fabricated snapshot.

use sea_orm::{ConnectionTrait, EntityTrait};

use super::{super::SnapshotError, Fixture, reference};

async fn assert_head_error(fixture: &Fixture) {
    let before = fixture.object_files();
    assert_eq!(
        before.len(),
        2,
        "fixture starts with only its tree and commit objects"
    );
    let result = fixture.snapshotter(&fixture.main).capture().await;
    let after = fixture.object_files();
    let error = result.expect_err("invalid authoritative HEAD must fail capture");
    assert!(
        matches!(&error, SnapshotError::Head(_)),
        "HEAD failure must not be masked by an unrelated capture error: {error}"
    );
    assert_eq!(
        after, before,
        "HEAD preflight failure must not create or change object files"
    );
}

#[tokio::test]
async fn capture_rejects_a_missing_authoritative_head_row() {
    // Given: a real repository schema, but no HEAD row or file.
    let fixture = Fixture::new().await;
    assert!(fixture.heads(&fixture.main.scope).await.is_empty());

    // When/Then: capture cannot invent an unborn main HEAD.
    assert_head_error(&fixture).await;
}

#[tokio::test]
async fn capture_rejects_duplicate_authoritative_head_rows() {
    // Given: only this temporary DB's main-scope constraint is removed.
    let fixture = Fixture::new().await;
    fixture
        .db
        .execute_unprepared("DROP INDEX idx_reference_head_main_unique")
        .await
        .expect("remove only the fixture's main HEAD constraint");
    let first = fixture
        .seed_head(&fixture.main.scope, Some("feature"), None)
        .await;
    let second = fixture
        .seed_head(&fixture.main.scope, Some("topic"), None)
        .await;
    let rows = fixture.heads(&fixture.main.scope).await;
    assert_eq!(rows.len(), 2, "fixture really has an ambiguous main HEAD");
    assert!(rows.contains(&first) && rows.contains(&second));

    // When/Then: neither the first row nor a fallback file resolves ambiguity.
    assert_head_error(&fixture).await;
}

#[tokio::test]
async fn capture_rejects_a_corrupt_detached_head_oid() {
    // Given: a unique detached HEAD row contains an invalid object id.
    let fixture = Fixture::new().await;
    let head = fixture
        .seed_head(&fixture.main.scope, None, Some("not-an-object-id"))
        .await;
    assert_eq!(fixture.heads(&fixture.main.scope).await, vec![head]);

    // When/Then: capture propagates corruption rather than choosing main.
    assert_head_error(&fixture).await;
}

#[tokio::test]
async fn capture_propagates_authoritative_head_query_failure() {
    // Given: the pinned temp repository's HEAD table becomes unavailable.
    let fixture = Fixture::new().await;
    fixture
        .seed_head(&fixture.main.scope, Some("feature"), None)
        .await;
    fixture
        .db
        .execute_unprepared("ALTER TABLE `reference` RENAME TO fixture_hidden_reference")
        .await
        .expect("hide only the fixture's reference table");
    assert!(reference::Entity::find().all(&fixture.db).await.is_err());

    // When/Then: database failure cannot authorize a fabricated HEAD snapshot.
    assert_head_error(&fixture).await;
}
