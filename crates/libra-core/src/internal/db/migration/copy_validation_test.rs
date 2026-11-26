//! Copy-first validation must prove row identity before dropping its source.

use sea_orm::{ConnectionTrait, Database, DbErr, TransactionTrait};

use super::validate_copy;

async fn validate_fixture(sql: &str, keys: &[&str]) -> Result<(), DbErr> {
    validate_fixture_with_fields(sql, keys, keys).await
}

async fn validate_fixture_with_fields(
    sql: &str,
    nonempty_fields: &[&str],
    comparison_fields: &[&str],
) -> Result<(), DbErr> {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let txn = conn.begin().await.unwrap();
    txn.execute_unprepared(sql).await.unwrap();
    let result = validate_copy(
        &txn,
        "source",
        "staging",
        nonempty_fields,
        comparison_fields,
    )
    .await;
    txn.rollback().await.unwrap();
    result
}

#[tokio::test]
async fn equal_counts_with_different_keys_are_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);
         INSERT INTO source VALUES ('original');
         INSERT INTO staging VALUES ('replacement');",
        &["id"],
    )
    .await
    .expect_err("equal row counts must not conceal a replaced key");
    assert!(error.to_string().contains("key-set mismatch"), "{error}");
}

#[tokio::test]
async fn crossed_composite_keys_are_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (a TEXT, b TEXT, PRIMARY KEY (a, b));
         CREATE TABLE staging (a TEXT, b TEXT, PRIMARY KEY (a, b));
         INSERT INTO source VALUES ('a', '1'), ('b', '2');
         INSERT INTO staging VALUES ('a', '2'), ('b', '1');",
        &["a", "b"],
    )
    .await
    .expect_err("matching per-column values must not conceal changed key tuples");
    assert!(error.to_string().contains("key-set mismatch"), "{error}");
}

async fn validate_view_refs(staging_remote: &str) -> Result<(), DbErr> {
    // Keep the production nonempty fields separate from complete row identity:
    // ref_remote is part of the primary key, but an empty remote is valid.
    validate_fixture_with_fields(
        &format!(
            "CREATE TABLE source (
               view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL,
               ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL,
               PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote));
             CREATE TABLE staging (
               view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL,
               ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL,
               PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote));
             INSERT INTO source VALUES ('view', 'branch', 'main', '', 'oid');
             INSERT INTO staging VALUES ('view', 'branch', 'main', '{staging_remote}', 'oid');"
        ),
        &["view_id", "ref_kind", "ref_name", "target_oid"],
        &[
            "view_id",
            "ref_kind",
            "ref_name",
            "ref_remote",
            "target_oid",
        ],
    )
    .await
}

#[tokio::test]
async fn changed_ref_remote_key_is_rejected() {
    let error = validate_view_refs("origin")
        .await
        .expect_err("ref_remote is part of row identity even though it may be empty");
    assert!(error.to_string().contains("key-set mismatch"), "{error}");
}

#[tokio::test]
async fn matching_empty_ref_remote_is_valid() {
    validate_view_refs("").await.unwrap();
}

#[tokio::test]
async fn missing_source_table_is_rejected() {
    validate_fixture("CREATE TABLE staging (id TEXT PRIMARY KEY);", &["id"])
        .await
        .expect_err("an absent source must not be treated as a verified empty copy");
}

#[tokio::test]
async fn missing_staging_table_is_rejected() {
    validate_fixture("CREATE TABLE source (id TEXT PRIMARY KEY);", &["id"])
        .await
        .expect_err("an absent staging table must not be treated as a verified empty copy");
}

#[tokio::test]
async fn both_tables_missing_is_rejected() {
    validate_fixture("SELECT 1;", &["id"])
        .await
        .expect_err("two missing tables do not prove a successful copy");
}

#[tokio::test]
async fn missing_source_key_column_is_rejected_even_when_empty() {
    validate_fixture(
        "CREATE TABLE source (wrong_column TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);",
        &["id"],
    )
    .await
    .expect_err("empty rows do not excuse a missing source key column");
}

#[tokio::test]
async fn missing_staging_key_column_is_rejected_even_when_empty() {
    validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (wrong_column TEXT PRIMARY KEY);",
        &["id"],
    )
    .await
    .expect_err("empty rows do not excuse a missing staging key column");
}

#[tokio::test]
async fn identical_key_sets_in_different_orders_are_valid() {
    validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);
         INSERT INTO source VALUES ('a'), ('b');
         INSERT INTO staging VALUES ('b'), ('a');",
        &["id"],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unequal_row_counts_are_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);
         INSERT INTO source VALUES ('a'), ('b');
         INSERT INTO staging VALUES ('a');",
        &["id"],
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("row-count mismatch"), "{error}");
}

#[tokio::test]
async fn empty_required_key_is_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);
         INSERT INTO source VALUES ('');
         INSERT INTO staging VALUES ('');",
        &["id"],
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("empty key"), "{error}");
}

#[tokio::test]
async fn missing_source_comparison_only_column_is_rejected_when_empty() {
    validate_fixture_with_fields(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY, ref_remote TEXT NOT NULL);",
        &["id"],
        &["id", "ref_remote"],
    )
    .await
    .expect_err("a comparison-only column must exist in the empty source");
}

#[tokio::test]
async fn missing_staging_comparison_only_column_is_rejected_when_empty() {
    validate_fixture_with_fields(
        "CREATE TABLE source (id TEXT PRIMARY KEY, ref_remote TEXT NOT NULL);
         CREATE TABLE staging (id TEXT PRIMARY KEY);",
        &["id"],
        &["id", "ref_remote"],
    )
    .await
    .expect_err("a comparison-only column must exist in the empty staging table");
}

#[tokio::test]
async fn matching_primary_key_with_changed_payload_is_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY, description TEXT NOT NULL);
         CREATE TABLE staging (id TEXT PRIMARY KEY, description TEXT NOT NULL);
         INSERT INTO source VALUES ('id', 'original');
         INSERT INTO staging VALUES ('id', 'replacement');",
        &["id", "description"],
    )
    .await
    .expect_err("payload comparisons must survive separation from nonempty fields");
    assert!(error.to_string().contains("key-set mismatch"), "{error}");
}

#[tokio::test]
async fn null_required_key_is_rejected() {
    let error = validate_fixture(
        "CREATE TABLE source (id TEXT PRIMARY KEY);
         CREATE TABLE staging (id TEXT PRIMARY KEY);
         INSERT INTO source VALUES (NULL);
         INSERT INTO staging VALUES (NULL);",
        &["id"],
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("empty key"), "{error}");
}
