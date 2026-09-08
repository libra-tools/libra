//! Real historical repository schemas, built forward without a current CLI init.

use std::{path::Path, time::Duration};

use libra::internal::db::{
    self,
    migration::{MigrationRunner, builtin_migrations},
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};

#[path = "historical_schema/state.rs"]
mod state;
pub(super) use state::{assert_current, assert_history, snapshot};

pub(super) async fn connect(repo: &Path) -> DatabaseConnection {
    let mut options = ConnectOptions::new(format!(
        "sqlite://{}",
        repo.join(".libra/libra.db").display()
    ));
    options
        .sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(options)
        .await
        .expect("connect raw historical repository")
}

pub(super) async fn repository_at(tip: i64) -> tempfile::TempDir {
    assert!(matches!(tip, 2026050601 | 2026072304 | 2026073101));
    let repo = tempfile::tempdir().expect("historical repository");
    let storage = repo.path().join(".libra");
    for directory in ["objects/pack", "objects/info", "info", "hooks"] {
        std::fs::create_dir_all(storage.join(directory)).unwrap();
    }
    std::fs::File::create(storage.join("libra.db")).unwrap();
    let conn = connect(repo.path()).await;
    conn.execute_unprepared(include_str!("../../sql/sqlite_20260309_init.sql"))
        .await
        .expect("historical bootstrap");
    db::ensure_ai_runtime_contract_schema(&conn)
        .await
        .expect("runtime contract tables");
    // The July rebuild consumes the lazy-era full shape. Do not introduce
    // those columns into the May fixture before they historically existed.
    if tip >= 2026072101 {
        conn.execute_unprepared(
            "ALTER TABLE rebase_state ADD COLUMN autosquash INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE rebase_state ADD COLUMN todo_actions TEXT NOT NULL DEFAULT ''; \
             ALTER TABLE rebase_state ADD COLUMN empty_mode TEXT NOT NULL DEFAULT 'keep';",
        )
        .await
        .expect("historical rebase normalization");
    }
    let mut runner = MigrationRunner::new();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version <= tip),
        )
        .expect("bounded historical registry");
    runner
        .run_pending(&conn)
        .await
        .expect("apply only historical migrations");
    conn.execute_unprepared(
        "INSERT INTO config_kv (key,value,encrypted) VALUES \
         ('core.repositoryformatversion','0',0),('core.filemode','false',0), \
         ('core.bare','false',0),('core.logallrefupdates','true',0), \
         ('core.ignorecase','false',0),('core.objectformat','sha1',0), \
         ('core.initrefformat','strict',0), \
         ('libra.repoid','cd5b6c7e-1f4a-4caa-9c34-0318be6ea8ef',0); \
         INSERT INTO reference (name,kind) VALUES ('main','Head'),('intent','Branch');",
    )
    .await
    .expect("seed historical repository identity and unborn HEAD");
    let traces = if tip < 2026062301 {
        "agent-traces"
    } else {
        "traces"
    };
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO reference (name,kind) VALUES (?, 'Branch')",
        [traces.into()],
    ))
    .await
    .expect("seed historically named capture branch");
    assert_history(&conn, tip).await;
    conn.close()
        .await
        .expect("close historical fixture before any CLI invocation");
    repo
}
