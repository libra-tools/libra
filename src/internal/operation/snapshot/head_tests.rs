//! Snapshot HEAD and content identity follow the pinned scope's SQLite row.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use support::Fixture;

use super::HeadState;
use crate::internal::model::reference::{self, ConfigKind};

#[path = "tests/head_tests/errors.rs"]
mod errors;
#[path = "tests/head_tests/support.rs"]
mod support;

#[tokio::test]
async fn capture_reads_born_feature_head_without_a_head_file() {
    // Given: the real branch and HEAD live only in the repository database.
    let fixture = Fixture::new().await;
    fixture.seed_branch("feature").await;
    let head = fixture
        .seed_head(&fixture.main.scope, Some("feature"), None)
        .await;
    assert_eq!(fixture.heads(&fixture.main.scope).await, vec![head]);

    // When: a caller captures its explicitly resolved main worktree.
    let snapshot = fixture
        .snapshotter(&fixture.main)
        .capture()
        .await
        .expect("capture");

    // Then: the non-default branch name survives without a sidecar file.
    assert_eq!(
        snapshot.snapshot.head,
        HeadState::Symbolic {
            reference: "refs/heads/feature".into()
        }
    );
    assert!(!fixture.main.gitdir.join("HEAD").exists());
}

#[tokio::test]
async fn capture_keeps_detached_head_and_its_root_without_a_head_file() {
    // Given: a valid detached commit is stored, with no branch or HEAD file.
    let fixture = Fixture::new().await;
    let oid = fixture.commit_oid;
    fixture
        .seed_head(&fixture.main.scope, None, Some(&oid.to_string()))
        .await;
    assert!(fixture.storage.exist(&oid));

    // When: capture serializes the database's detached HEAD.
    let snapshot = fixture
        .snapshotter(&fixture.main)
        .capture()
        .await
        .expect("capture");

    // Then: the manifest names the commit as HEAD and as a retained root.
    assert_eq!(snapshot.snapshot.head, HeadState::Detached { oid });
    assert!(snapshot.snapshot.roots().contains(&oid));
    assert!(!fixture.main.gitdir.join("HEAD").exists());
}

#[tokio::test]
async fn capture_ignores_a_stale_symbolic_file_when_sqlite_head_is_detached() {
    // Given: a legacy file contradicts the authoritative detached row.
    let fixture = Fixture::new().await;
    let oid = fixture.commit_oid;
    fixture
        .seed_head(&fixture.main.scope, None, Some(&oid.to_string()))
        .await;
    let stale = b"ref: refs/heads/main\n";
    std::fs::write(fixture.main.gitdir.join("HEAD"), stale).expect("stale HEAD sidecar");

    // When: capture observes both representations.
    let snapshot = fixture
        .snapshotter(&fixture.main)
        .capture()
        .await
        .expect("capture");

    // Then: neither HEAD nor its roots are replaced by the stale file.
    assert_eq!(snapshot.snapshot.head, HeadState::Detached { oid });
    assert!(snapshot.snapshot.roots().contains(&oid));
    assert_eq!(
        std::fs::read(fixture.main.gitdir.join("HEAD")).expect("sidecar"),
        stale
    );
}

#[tokio::test]
async fn capture_preserves_non_default_unborn_topic_head() {
    // Given: an unborn topic has a valid HEAD row, but no Branch row or file.
    let fixture = Fixture::new().await;
    fixture
        .seed_head(&fixture.main.scope, Some("topic"), None)
        .await;
    let branches = reference::Entity::find()
        .filter(reference::Column::Kind.eq(ConfigKind::Branch))
        .all(&fixture.db)
        .await
        .expect("inspect unborn branches");
    assert!(branches.is_empty());

    // When: the unborn workspace is captured.
    let snapshot = fixture
        .snapshotter(&fixture.main)
        .capture()
        .await
        .expect("capture");

    // Then: absence of a commit never invents the default branch.
    assert_eq!(
        snapshot.snapshot.head,
        HeadState::Symbolic {
            reference: "refs/heads/topic".into()
        }
    );
}

#[tokio::test]
async fn capture_resolves_independent_main_and_linked_heads_in_one_database() {
    // Given: real linked markers resolve to one common DB and two private HEAD rows.
    let fixture = Fixture::new().await;
    let linked = fixture.linked_scope();
    fixture.seed_branch("feature").await;
    fixture.seed_branch("linked-topic").await;
    fixture
        .seed_head(&fixture.main.scope, Some("feature"), None)
        .await;
    fixture
        .seed_head(&linked.scope, Some("linked-topic"), None)
        .await;
    assert_eq!(fixture.heads(&fixture.main.scope).await.len(), 1);
    assert_eq!(fixture.heads(&linked.scope).await.len(), 1);

    // When: each independently pinned worktree captures from the same database.
    let main = fixture
        .snapshotter(&fixture.main)
        .capture()
        .await
        .expect("main capture");
    let linked_snapshot = fixture
        .snapshotter(&linked)
        .capture()
        .await
        .expect("linked capture");

    // Then: neither scope reads the other's HEAD or needs a HEAD sidecar.
    assert_eq!(
        main.snapshot.head,
        HeadState::Symbolic {
            reference: "refs/heads/feature".into()
        }
    );
    assert_eq!(
        linked_snapshot.snapshot.head,
        HeadState::Symbolic {
            reference: "refs/heads/linked-topic".into()
        }
    );
    assert_eq!(main.snapshot.workspace_id, "main");
    assert_eq!(
        linked_snapshot.snapshot.workspace_id,
        linked.scope.worktree_id().expect("linked id")
    );
    assert!(!fixture.main.gitdir.join("HEAD").exists());
    assert!(!linked.gitdir.join("HEAD").exists());
}

#[tokio::test]
async fn head_only_database_change_changes_snapshot_content_identity() {
    // Given: both branches already exist; the index, files and generation stay fixed.
    let fixture = Fixture::new().await;
    fixture.seed_branch("feature").await;
    fixture.seed_branch("topic").await;
    let head = fixture
        .seed_head(&fixture.main.scope, Some("feature"), None)
        .await;
    let mut snapshotter = fixture.snapshotter(&fixture.main);
    let before = snapshotter.capture().await.expect("baseline capture");
    snapshotter.pointer.last_snapshot_oid = before.snapshot_oid;
    snapshotter.pointer.last_content_oid = Some(before.content_oid);
    let unchanged = snapshotter.capture().await.expect("unchanged capture");
    assert_eq!(unchanged.content_oid, before.content_oid);
    assert!(!unchanged.changed);

    // When: an external writer changes only the authoritative HEAD row.
    let mut updated: reference::ActiveModel = head.into();
    updated.name = Set(Some("topic".into()));
    updated
        .update(&fixture.db)
        .await
        .expect("HEAD-only explicit database update");
    let after = snapshotter
        .capture()
        .await
        .expect("capture after HEAD-only drift");

    // Then: freshness can see the drift without a refs-facet or filesystem change.
    assert_ne!(
        after.content_oid, before.content_oid,
        "HEAD-only drift must change content identity"
    );
    assert!(after.changed);
    assert_eq!(
        after.snapshot.head,
        HeadState::Symbolic {
            reference: "refs/heads/topic".into()
        }
    );
    let mut without_head_change = after.snapshot;
    without_head_change.head = before.snapshot.head.clone();
    assert_eq!(
        without_head_change, before.snapshot,
        "all non-HEAD content is unchanged"
    );
    assert!(!fixture.main.gitdir.join("HEAD").exists());
}
