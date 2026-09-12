//! Additive legacy config repair and rollback safety (#472).

use sea_orm::EntityTrait;

use super::*;

// #472 shipped independently of operation v2. Its down/up contract belongs
// to that historical registry, before either forward-only operation barrier.
fn legacy_config_runner() -> Result<MigrationRunner, MigrationError> {
    let mut runner = MigrationRunner::new();
    runner.extend(
        builtin_migrations()
            .into_iter()
            .filter(|migration| migration.version < 2026090101 || migration.version == 2026090601),
    )?;
    Ok(runner)
}

#[tokio::test]
async fn opening_old_database_repairs_missing_legacy_config_and_preserves_values() {
    // Given a previously migrated database missing only its legacy table.
    let (_dir, url, path) = fresh_db_url();
    let conn = connect(&url).await;
    historical_bootstrap::initialize(&conn).await;
    super::builtin_runner()
        .unwrap()
        .run_pending(&conn)
        .await
        .unwrap();
    historical_bootstrap::assert_pre_v2_history(&conn).await;
    conn.execute_unprepared(
        "DROP TABLE config; \
         INSERT INTO config_kv (key, value, encrypted) VALUES ('init.defaultBranch', 'trunk', 0)",
    )
    .await
    .unwrap();
    conn.close().await.unwrap();

    // When a normal production connection opens the old database.
    let conn = libra::internal::db::establish_connection(path.to_str().unwrap())
        .await
        .unwrap();

    // Then legacy readers work again and modern config is preserved.
    assert!(table_exists(&conn, "config").await);
    let value = libra::internal::config::ConfigKv::get_with_conn(&conn, "init.defaultBranch")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value.value, "trunk");
    assert!(
        libra::internal::model::config::Entity::find()
            .all(&conn)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        all_builtin_runner()
            .unwrap()
            .current_version(&conn)
            .await
            .unwrap(),
        all_builtin_runner().unwrap().max_registered_version()
    );
}

#[tokio::test]
async fn legacy_config_migration_preserves_preexisting_rows_through_rollback() {
    // Given legacy values that predate the repair migration.
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    conn.execute_unprepared(include_str!(
        "../../sql/migrations/2026090601_legacy_config_table.sql"
    ))
    .await
    .unwrap();
    conn.execute_unprepared(
        "INSERT INTO config (configuration, name, key, value) \
         VALUES ('remote', 'origin', 'url', 'https://example.com/repo')",
    )
    .await
    .unwrap();
    let runner = legacy_config_runner().unwrap();

    // When the repair runs and a downgrade is subsequently requested.
    runner.run_pending(&conn).await.unwrap();
    assert_eq!(
        runner.rollback_to(&conn, 2026082401).await.unwrap(),
        vec![2026090601]
    );

    // Then older binaries retain their bootstrap table and every legacy value.
    let rows = libra::internal::model::config::Entity::find()
        .all(&conn)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, "https://example.com/repo");
    assert_eq!(
        runner.current_version(&conn).await.unwrap(),
        Some(2026082401)
    );
    assert_eq!(runner.run_pending(&conn).await.unwrap(), vec![2026090601]);
    assert_eq!(
        libra::internal::model::config::Entity::find()
            .all(&conn)
            .await
            .unwrap(),
        rows
    );
}

#[tokio::test]
async fn empty_legacy_config_can_roll_back_and_reapply() {
    // Given a migrated database with no legacy values to lose.
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = legacy_config_runner().unwrap();
    runner.run_pending(&conn).await.unwrap();

    // When rolling back the repair and reapplying it.
    assert_eq!(
        runner.rollback_to(&conn, 2026082401).await.unwrap(),
        vec![2026090601]
    );
    assert!(table_exists(&conn, "config").await);
    assert_eq!(runner.run_pending(&conn).await.unwrap(), vec![2026090601]);

    // Then legacy storage remains available throughout the downgrade and upgrade.
    assert!(table_exists(&conn, "config").await);
}
