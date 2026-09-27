//! Role-scoped migration boundaries, using only fixture-owned SQLite files.

use std::{collections::BTreeSet, path::Path};

use libra::{
    internal::db::{
        self, DatabaseRole, SchemaCompatibility,
        schema::{self, SchemaLedger, SchemaTopUp},
    },
    utils::test::ConfigDbFixture,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use serial_test::serial;

async fn raw_connection(path: &Path) -> DatabaseConnection {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let mut options = ConnectOptions::new(format!("sqlite://{}", path.display()));
    options
        .sqlx_logging(false)
        .map_sqlx_sqlite_pool_opts(|pool| pool.idle_timeout(None).max_lifetime(None));
    Database::connect(options).await.unwrap()
}

async fn tables(conn: &DatabaseConnection) -> BTreeSet<String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get_by_index(0).unwrap())
    .collect()
}

async fn text_rows(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by_index(0).unwrap())
        .collect()
}

#[test]
fn database_role_manifest_is_total_and_disjoint() {
    let manifest = schema::schema_manifest();
    let mut identities = BTreeSet::new();
    for entry in &manifest.migrations {
        assert!(!entry.roles.is_empty());
        assert!(!entry.roles.contains(&DatabaseRole::Derived));
        assert_eq!(
            entry
                .roles
                .iter()
                .filter(|role| **role == DatabaseRole::Repository)
                .count(),
            usize::from(entry.roles == [DatabaseRole::Repository])
        );
        for role in entry.roles {
            assert!(identities.insert((role.to_string(), entry.migration.version)));
        }
    }
    for entry in manifest.bootstraps {
        assert!(!entry.sql.is_empty());
        assert!(!entry.roles.is_empty());
        assert!(!entry.roles.contains(&DatabaseRole::Derived));
    }
    assert_eq!(manifest.top_ups.len(), 5);
    for role in [DatabaseRole::GlobalConfig, DatabaseRole::SystemConfig] {
        assert_eq!(
            schema::ledger_for_role(role).unwrap(),
            SchemaLedger::Configuration
        );
        assert_eq!(
            schema::top_ups_for_role(role).collect::<Vec<_>>(),
            [SchemaTopUp::ConfigKv]
        );
        assert_eq!(schema::migrations_for_role(role).len(), 1);
        assert_eq!(
            schema::latest_schema_version_for_role(role).unwrap(),
            Some(2026090601)
        );
    }
    assert!(schema::migrations_for_role(DatabaseRole::Repository).len() > 50);
    assert_eq!(
        db::migration::builtin_migrations(),
        schema::migrations_for_role(DatabaseRole::Repository)
    );
    assert!(schema::ledger_for_role(DatabaseRole::Derived).is_err());
    assert_eq!(
        schema::bootstraps_for_role(DatabaseRole::Derived).count(),
        0
    );
    assert_eq!(schema::top_ups_for_role(DatabaseRole::Derived).count(), 0);
    assert!(schema::latest_schema_version_for_role(DatabaseRole::Derived).is_err());
    assert!(std::ptr::eq(
        schema::schema_manifest(),
        schema::schema_manifest()
    ));
}

#[tokio::test]
#[serial(env)]
async fn role_scoped_schema_writer_keeps_ledgers_disjoint() {
    let fixture = ConfigDbFixture::new().unwrap();
    for (role, path) in [
        (DatabaseRole::Repository, fixture.root().join("repo.db")),
        (
            DatabaseRole::GlobalConfig,
            fixture.global_db().to_path_buf(),
        ),
        (
            DatabaseRole::SystemConfig,
            fixture.system_db().to_path_buf(),
        ),
    ] {
        // No metadata inspection may silently create an absent file.
        assert_eq!(
            db::inspect_database_schema_for_role(&path, role)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!path.exists());
        let conn = raw_connection(&path).await;
        assert!(matches!(
            schema::inspect_schema_for_connection(&conn, role)
                .await
                .unwrap(),
            SchemaCompatibility::UpgradeRequired {
                current_version: None,
                ..
            }
        ));
        assert!(tables(&conn).await.is_empty());
        conn.close().await.unwrap();
        let conn = db::establish_connection_for_role(path.to_str().unwrap(), role)
            .await
            .unwrap();
        let latest = schema::latest_schema_version_for_role(role)
            .unwrap()
            .unwrap();
        assert_eq!(
            schema::current_schema_version_for_role(&conn, role)
                .await
                .unwrap(),
            Some(latest)
        );
        let memory_tables: BTreeSet<String> = tables(&conn)
            .await
            .into_iter()
            .filter(|name| name.starts_with("memory_"))
            .collect();
        let expected_memory_tables = if role == DatabaseRole::Repository {
            [
                "memory_episode",
                "memory_episode_evidence",
                "memory_episode_path",
                "memory_episode_search_doc",
                "memory_projection_state",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(memory_tables, expected_memory_tables, "{role}");
        let table = schema::ledger_for_role(role).unwrap().table_name();
        assert_eq!(
            text_rows(
                &conn,
                &format!("SELECT name FROM {table} WHERE version IN (2026092601,2026092602) ORDER BY version")
            )
            .await,
            if role == DatabaseRole::Repository {
                vec!["memory_core".to_owned(), "memory_path_search".to_owned()]
            } else {
                vec![]
            },
            "{role} Memory receipt isolation"
        );
        let other = if role == DatabaseRole::Repository {
            "configuration_schema_versions"
        } else {
            "schema_versions"
        };
        assert!(!tables(&conn).await.contains(other));
        conn.execute_unprepared(&format!(
            "INSERT INTO {table} (version,name,applied_at) VALUES ({},'future','fixture')",
            latest + 1
        ))
        .await
        .unwrap();
        conn.close().await.unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            db::inspect_database_schema_for_role(&path, role)
                .await
                .unwrap(),
            SchemaCompatibility::UnsupportedFuture { .. }
        ));
        assert!(
            db::establish_connection_for_role(path.to_str().unwrap(), role)
                .await
                .is_err()
        );
        assert!(
            db::upgrade_database_schema_for_role(&path, role)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "future schema paths must not write"
        );
    }
    let path = fixture.root().join("derived.db");
    assert!(
        db::create_database_for_role(path.to_str().unwrap(), DatabaseRole::Derived)
            .await
            .is_err()
    );
    assert!(
        db::establish_connection_for_role(path.to_str().unwrap(), DatabaseRole::Derived)
            .await
            .is_err()
    );
    assert!(
        db::inspect_database_schema_for_role(&path, DatabaseRole::Derived)
            .await
            .is_err()
    );
    assert!(
        db::upgrade_database_schema_for_role(&path, DatabaseRole::Derived)
            .await
            .is_err()
    );
    assert!(!path.exists());
}

#[tokio::test]
#[serial(env)]
async fn global_and_system_bootstrap_create_no_repository_shape() {
    let fixture = ConfigDbFixture::new().unwrap();
    for (role, path) in [
        (DatabaseRole::GlobalConfig, fixture.global_db()),
        (DatabaseRole::SystemConfig, fixture.system_db()),
    ] {
        let conn = db::create_database_for_role(path.to_str().unwrap(), role)
            .await
            .unwrap();
        assert_eq!(
            tables(&conn).await,
            ["config", "config_kv", "configuration_schema_versions"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        conn.execute_unprepared(
            "INSERT INTO config_kv (key,value,encrypted) VALUES ('fixture','preserved',0)",
        )
        .await
        .unwrap();
        let receipts = text_rows(&conn, "SELECT version || ':' || name || ':' || applied_at FROM configuration_schema_versions ORDER BY version").await;
        assert!(
            schema::upgrade_connection_for_role(&conn, role)
                .await
                .unwrap()
                .applied_versions
                .is_empty()
        );
        assert_eq!(text_rows(&conn, "SELECT version || ':' || name || ':' || applied_at FROM configuration_schema_versions ORDER BY version").await, receipts);
        conn.close().await.unwrap();
        let before = std::fs::read(path).unwrap();
        assert!(
            db::create_database_for_role(path.to_str().unwrap(), role)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "create must not overwrite an existing database"
        );

        // Independent pools model two processes opening the same legacy file.
        // The under-lock version check must prevent a second receipt/DDL run.
        let concurrent_path = fixture.root().join(format!("concurrent-{role}.db"));
        let first = raw_connection(&concurrent_path).await;
        let second = raw_connection(&concurrent_path).await;
        let (first_report, second_report) = tokio::join!(
            schema::upgrade_connection_for_role(&first, role),
            schema::upgrade_connection_for_role(&second, role),
        );
        let first_report = first_report.unwrap();
        let second_report = second_report.unwrap();
        assert_eq!(
            first_report.applied_versions.len() + second_report.applied_versions.len(),
            1
        );
        assert_eq!(first_report.current_version, second_report.current_version);
        assert_eq!(
            text_rows(&first, "SELECT name FROM configuration_schema_versions").await,
            ["configuration_base"]
        );
        first.close().await.unwrap();
        second.close().await.unwrap();
    }
}

#[tokio::test]
#[serial(env)]
async fn repository_only_upgrade_preserves_config_receipt_and_values() {
    let fixture = ConfigDbFixture::new().unwrap();
    for (role, path) in [
        (DatabaseRole::GlobalConfig, fixture.global_db()),
        (DatabaseRole::SystemConfig, fixture.system_db()),
    ] {
        let conn = db::create_database_for_role(path.to_str().unwrap(), role)
            .await
            .unwrap();
        conn.execute_unprepared("WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<1000) INSERT INTO config_kv (key,value,encrypted) SELECT 'key-'||v, 'value-'||v, v%2 FROM n").await.unwrap();
        // Replace the value surface with a view that errors on access. Schema
        // inspection must use ledger metadata, not scan even one config row.
        conn.execute_unprepared("ALTER TABLE config_kv RENAME TO fixture_values; CREATE VIEW config_kv AS SELECT missing_column FROM fixture_values").await.unwrap();
        assert!(matches!(
            schema::inspect_schema_for_connection(&conn, role)
                .await
                .unwrap(),
            SchemaCompatibility::Compatible { .. }
        ));
        conn.close().await.unwrap();
    }
    let global_before = std::fs::read(fixture.global_db()).unwrap();
    let system_before = std::fs::read(fixture.system_db()).unwrap();
    let path = fixture.root().join("repo.db");
    let conn = db::create_database(path.to_str().unwrap()).await.unwrap();
    assert!(tables(&conn).await.contains("object_index"));
    assert!(
        !tables(&conn)
            .await
            .contains("configuration_schema_versions")
    );
    conn.close().await.unwrap();
    db::upgrade_database_schema(&path).await.unwrap();
    assert_eq!(std::fs::read(fixture.global_db()).unwrap(), global_before);
    assert_eq!(std::fs::read(fixture.system_db()).unwrap(), system_before);
}

#[tokio::test]
#[serial(env)]
async fn role_schema_up_fault_rolls_back_ledger() {
    let fixture = ConfigDbFixture::new().unwrap();
    for (role, path) in [
        (DatabaseRole::GlobalConfig, fixture.global_db()),
        (DatabaseRole::SystemConfig, fixture.system_db()),
    ] {
        let conn = raw_connection(path).await;
        // A corrupt old table causes the config_kv index top-up to fail after
        // the new ledger and legacy-config DDL have executed in the transaction.
        conn.execute_unprepared("CREATE TABLE config_kv (id INTEGER PRIMARY KEY, sentinel TEXT); INSERT INTO config_kv VALUES (1,'preserved')").await.unwrap();
        assert!(
            schema::upgrade_connection_for_role(&conn, role)
                .await
                .is_err()
        );
        assert_eq!(
            tables(&conn).await,
            ["config_kv".to_owned()].into_iter().collect()
        );
        assert_eq!(
            text_rows(&conn, "SELECT sentinel FROM config_kv").await,
            ["preserved"]
        );
        conn.execute_unprepared("DROP TABLE config_kv; CREATE TABLE configuration_schema_versions (version INTEGER PRIMARY KEY,name TEXT NOT NULL,applied_at TEXT NOT NULL); CREATE TRIGGER reject_receipt BEFORE INSERT ON configuration_schema_versions BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;").await.unwrap();
        assert!(
            schema::upgrade_connection_for_role(&conn, role)
                .await
                .is_err()
        );
        assert_eq!(
            tables(&conn).await,
            ["configuration_schema_versions".to_owned()]
                .into_iter()
                .collect()
        );
        assert_eq!(
            schema::current_schema_version_for_role(&conn, role)
                .await
                .unwrap(),
            None
        );
        conn.execute_unprepared("DROP TRIGGER reject_receipt")
            .await
            .unwrap();
        assert_eq!(
            schema::upgrade_connection_for_role(&conn, role)
                .await
                .unwrap()
                .applied_versions,
            [2026090601]
        );
        conn.close().await.unwrap();
    }
}

#[tokio::test]
#[serial(env)]
async fn configuration_ledger_has_independent_version_namespace() {
    let fixture = ConfigDbFixture::new().unwrap();
    let conn = db::create_database(fixture.global_db().to_str().unwrap())
        .await
        .unwrap();
    let old_receipts = text_rows(
        &conn,
        "SELECT version || ':' || name || ':' || applied_at FROM schema_versions ORDER BY version",
    )
    .await;
    let old_latest = schema::current_schema_version_for_role(&conn, DatabaseRole::Repository)
        .await
        .unwrap();
    assert!(old_latest.unwrap() > 2026090601);
    conn.execute_unprepared(
        "INSERT INTO config_kv (key,value,encrypted) VALUES ('fixture','unchanged',1)",
    )
    .await
    .unwrap();
    let report = schema::upgrade_connection_for_role(&conn, DatabaseRole::GlobalConfig)
        .await
        .unwrap();
    assert_eq!(report.previous_version, None);
    assert_eq!(report.current_version, Some(2026090601));
    assert_eq!(
        schema::current_schema_version_for_role(&conn, DatabaseRole::Repository)
            .await
            .unwrap(),
        old_latest
    );
    assert_eq!(text_rows(&conn, "SELECT version || ':' || name || ':' || applied_at FROM schema_versions ORDER BY version").await, old_receipts);
    assert_eq!(
        text_rows(
            &conn,
            "SELECT name FROM schema_versions WHERE version=2026090601"
        )
        .await,
        ["legacy_config_table"]
    );
    assert_eq!(
        text_rows(
            &conn,
            "SELECT name FROM configuration_schema_versions WHERE version=2026090601"
        )
        .await,
        ["configuration_base"]
    );
    assert_eq!(
        text_rows(
            &conn,
            "SELECT value || ':' || encrypted FROM config_kv WHERE key='fixture'"
        )
        .await,
        ["unchanged:1"]
    );
    conn.close().await.unwrap();
}
