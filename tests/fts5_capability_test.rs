//! Release-profile capability proof for Libra's linked SQLite FTS5 build.

use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};

#[tokio::test]
async fn sqlite_fts5_release_capability() {
    let database = Database::connect("sqlite::memory:")
        .await
        .expect("connect with Libra's linked SQLite");
    let enabled: i64 = database
        .query_one_raw(Statement::from_string(
            database.get_database_backend(),
            "SELECT sqlite_compileoption_used('ENABLE_FTS5') AS enabled".to_string(),
        ))
        .await
        .expect("read SQLite compile options")
        .expect("compile-option row")
        .try_get("", "enabled")
        .expect("compile-option value");
    assert_eq!(enabled, 1, "the release-linked SQLite must compile FTS5 in");

    database
        .execute_unprepared(
            "CREATE TABLE capability_doc (
                rowid INTEGER PRIMARY KEY,
                goal TEXT NOT NULL,
                summary TEXT NOT NULL,
                decisions TEXT NOT NULL,
                failed_attempts TEXT NOT NULL,
                unresolved TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE capability_fts USING fts5(
                goal, summary, decisions, failed_attempts, unresolved,
                content='capability_doc', content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
             );",
        )
        .await
        .expect("create the production-shape external-content FTS5 schema");

    let transaction = database.begin().await.expect("begin capability insert");
    transaction
        .execute_unprepared(
            "INSERT INTO capability_doc VALUES
                (1, 'needle', '', '', '', ''),
                (2, '', '', '', '', 'needle'),
                (3, 'café', '', '', '', '');
             INSERT INTO capability_fts (
                rowid, goal, summary, decisions, failed_attempts, unresolved
             ) SELECT
                rowid, goal, summary, decisions, failed_attempts, unresolved
               FROM capability_doc;",
        )
        .await
        .expect("insert content and matching postings");
    transaction.commit().await.expect("commit capability rows");

    let ranked = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT rowid,
                    bm25(capability_fts, 8.0, 5.0, 4.0, 3.0, 2.0) AS score
               FROM capability_fts
              WHERE capability_fts MATCH ?
              ORDER BY score ASC, rowid ASC",
            ["needle".into()],
        ))
        .await
        .expect("execute parameter-bound MATCH and BM25");
    let ranked_ids: Vec<i64> = ranked
        .iter()
        .map(|row| row.try_get("", "rowid").expect("ranked rowid"))
        .collect();
    assert_eq!(
        ranked_ids,
        vec![1, 2],
        "a goal hit with weight 8 must sort before an unresolved hit with weight 2"
    );
    let scores: Vec<f64> = ranked
        .iter()
        .map(|row| row.try_get("", "score").expect("BM25 score"))
        .collect();
    assert!(
        scores[0] < scores[1],
        "SQLite BM25 ranks smaller scores first"
    );

    let diacritic_matches: i64 = database
        .query_one_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT COUNT(*) AS count FROM capability_fts
              WHERE capability_fts MATCH ?",
            ["cafe".into()],
        ))
        .await
        .expect("query unicode61 diacritic behavior")
        .expect("diacritic count row")
        .try_get("", "count")
        .expect("diacritic count");
    assert_eq!(
        diacritic_matches, 1,
        "remove_diacritics=2 must make cafe match café"
    );
}
