//! Explicit-connection HEAD reads never borrow cwd or thread-local hash configuration.

use sea_orm::{DatabaseConnection, DbBackend, Statement, TransactionTrait};

use super::*;
use crate::internal::db::create_database;

fn target_scope() -> WorktreeScope {
    WorktreeScope::Linked("target".into())
}

async fn fixture(
    format: Option<&str>,
    name: Option<&str>,
    commit: Option<&str>,
) -> (tempfile::TempDir, DatabaseConnection) {
    let directory = tempfile::tempdir().expect("explicit HEAD database");
    let path = directory.path().join("repository.db");
    let db = create_database(path.to_str().expect("database path"))
        .await
        .expect("repository schema");
    if let Some(format) = format {
        ConfigKv::set_with_conn(&db, "core.objectformat", format, false)
            .await
            .expect("repository format");
    }
    reference::ActiveModel {
        name: Set(name.map(str::to_string)),
        commit: Set(commit.map(str::to_string)),
        kind: Set(reference::ConfigKind::Head),
        remote: Set(None),
        worktree_id: Set(Some("target".into())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("authoritative HEAD row");
    (directory, db)
}

#[tokio::test]
async fn scoped_head_reads_both_hash_algorithms_independently_of_ambient_kind() {
    let ambient = git_internal::hash::get_hash_kind();
    let mut mismatches = 0;
    for kind in [HashKind::Sha1, HashKind::Sha256] {
        let raw = "1".repeat(kind.hex_len());
        let (_directory, db) = fixture(Some(kind.as_str()), None, Some(&raw)).await;
        let head = Head::current_for_scope_result_with_conn(&db, &target_scope())
            .await
            .expect("explicit repository hash kind");
        assert!(
            matches!(head, Head::Detached(oid) if oid.kind() == kind && oid.to_string() == raw)
        );
        mismatches += usize::from(kind != ambient);
        assert_eq!(git_internal::hash::get_hash_kind(), ambient);
    }
    assert_eq!(mismatches, 1, "one case must differ from ambient TLS");
}

#[tokio::test]
async fn scoped_head_rejects_detached_oid_from_a_different_repository_algorithm() {
    for (format, length) in [("sha1", 64), ("sha256", 40)] {
        let raw = "2".repeat(length);
        let (_directory, db) = fixture(Some(format), None, Some(&raw)).await;
        let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
            .await
            .expect_err("wrong repository algorithm");
        assert!(matches!(error, BranchStoreError::Corrupt { .. }));
        assert!(error.to_string().contains("but this repository uses"));
    }
}

#[tokio::test]
async fn scoped_head_rejects_unknown_or_noncanonical_object_format() {
    for format in ["sha512", "SHA1", " sha1 ", ""] {
        let (_directory, db) = fixture(Some(format), Some("unborn"), None).await;
        let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
            .await
            .expect_err("unsupported object format");
        assert!(
            matches!(&error, BranchStoreError::Corrupt { name, .. } if name == "core.objectformat")
        );
        assert_eq!(
            error.to_string(),
            format!(
                "stored branch reference 'core.objectformat' is corrupt: unsupported repository object format '{format}'; expected 'sha1' or 'sha256'"
            )
        );
    }
}

#[tokio::test]
async fn scoped_head_missing_object_format_defaults_only_to_sha1() {
    let raw = "3".repeat(40);
    let (_directory, db) = fixture(None, None, Some(&raw)).await;
    let head = Head::current_for_scope_result_with_conn(&db, &target_scope())
        .await
        .expect("legacy SHA-1 repository");
    assert!(matches!(head, Head::Detached(oid) if oid.kind() == HashKind::Sha1));
}

#[tokio::test]
async fn scoped_head_config_query_failure_preserves_the_underlying_cause() {
    let (_directory, db) = fixture(None, Some("unborn"), None).await;
    db.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        "DROP TABLE config_kv",
    ))
    .await
    .expect("damage only the explicit fixture database");
    let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
        .await
        .expect_err("a config query failure must not default to SHA-1");
    assert!(matches!(error, BranchStoreError::Query(_)));
    let message = error.to_string();
    assert!(
        message.contains("failed to read core.objectformat for HEAD"),
        "{message}"
    );
    assert!(message.contains("no such table: config_kv"), "{message}");
}

#[tokio::test]
async fn scoped_head_rejects_empty_symbolic_names_with_stable_diagnostics() {
    // Unlike a zero-length name, these pass the real database's name <> '' check.
    for name in [" ", " \t"] {
        let (_directory, db) = fixture(None, Some(name), None).await;
        let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
            .await
            .expect_err("empty symbolic HEAD");
        assert_eq!(
            error.to_string(),
            "stored branch reference 'HEAD' is corrupt: symbolic HEAD branch name is empty"
        );
    }
}

#[test]
fn scoped_head_decoder_rejects_a_zero_length_symbolic_name() {
    // Exercise decoder defense directly without disabling the database's empty-name check.
    let model = reference::Model {
        id: 1,
        name: Some(String::new()),
        kind: reference::ConfigKind::Head,
        commit: None,
        remote: None,
        worktree_id: Some("target".into()),
    };
    let error = decode_local_head(model, HashKind::Sha1).expect_err("empty symbolic HEAD");
    assert_eq!(
        error.to_string(),
        "stored branch reference 'HEAD' is corrupt: symbolic HEAD branch name is empty"
    );
}

#[tokio::test]
async fn scoped_head_missing_detached_commit_returns_a_stable_error() {
    let (_directory, db) = fixture(None, None, None).await;
    let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
        .await
        .expect_err("missing detached commit");
    assert_eq!(
        error.to_string(),
        "stored branch reference 'HEAD' is corrupt: detached HEAD is missing commit hash"
    );
}

#[tokio::test]
async fn scoped_head_rejects_malformed_detached_commit() {
    let (_directory, db) = fixture(None, None, Some(&"z".repeat(40))).await;
    let error = Head::current_for_scope_result_with_conn(&db, &target_scope())
        .await
        .expect_err("malformed detached commit");
    assert!(
        error
            .to_string()
            .contains("invalid detached HEAD commit hash")
    );
}

#[tokio::test]
async fn scoped_head_uses_the_exact_database_and_scope_including_transactions() {
    let (_first_directory, first) = fixture(None, Some("unborn-first"), None).await;
    let (_second_directory, second) = fixture(None, Some("unborn-second"), None).await;
    let transaction = first.begin().await.expect("explicit transaction");
    let head = Head::current_for_scope_result_with_conn(&transaction, &target_scope())
        .await
        .expect("unborn branch requires no branch tip row");
    assert!(matches!(head, Head::Branch(name) if name == "unborn-first"));
    transaction.rollback().await.expect("read transaction");
    let head = Head::current_for_scope_result_with_conn(&second, &target_scope())
        .await
        .expect("other database");
    assert!(matches!(head, Head::Branch(name) if name == "unborn-second"));
    let missing = Head::current_for_scope_result_with_conn(&second, &WorktreeScope::Main)
        .await
        .expect_err("missing scope must not select another scope's row");
    assert!(matches!(missing, BranchStoreError::Corrupt { .. }));
    assert!(missing.to_string().contains("HEAD reference is missing"));
}
