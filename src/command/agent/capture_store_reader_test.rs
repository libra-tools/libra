//! CQ-03: command readers of the capture catalog use `.libra/private/capture.db`.

use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use super::{
    CheckpointListArgs, CheckpointSubcommand, CleanArgs, DoctorArgs,
    graph::GraphArgs,
    session::{SessionListArgs, SessionSubcommand},
    skill::{SkillSearchArgs, SkillSubcommand},
    status::StatusArgs,
    workspace::{WorkspaceListArgs, WorkspaceSubcommand},
};
use crate::utils::{
    output::{ColorChoice, JsonFormat, OutputConfig, ProgressMode, ProgressPreference},
    test::ChangeDirGuard,
};

fn json_output() -> OutputConfig {
    OutputConfig {
        json_format: Some(JsonFormat::Compact),
        color: ColorChoice::Never,
        pager: false,
        quiet: true,
        exit_code_on_warning: false,
        progress: ProgressMode::None,
        progress_preference: ProgressPreference::None,
    }
}

async fn drop_repository_capture_tables(conn: &impl ConnectionTrait) {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        "PRAGMA foreign_keys = OFF".to_string(),
    ))
    .await
    .expect("disable foreign keys before dropping capture tables");
    let names = conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT type, name FROM sqlite_master \
             WHERE type IN ('table', 'trigger') \
               AND (name LIKE 'agent\\_%' ESCAPE '\\' \
                    OR tbl_name LIKE 'agent\\_%' ESCAPE '\\')"
                .to_string(),
        ))
        .await
        .expect("list repository capture tables");
    assert!(
        !names.is_empty(),
        "the repository fixture must start with capture tables"
    );
    for row in names {
        let kind: String = row.try_get_by("type").expect("object type");
        let name: String = row.try_get_by("name").expect("object name");
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
            "unexpected capture object name {name}"
        );
        let statement = match kind.as_str() {
            "table" => format!("DROP TABLE \"{name}\""),
            "trigger" => format!("DROP TRIGGER IF EXISTS \"{name}\""),
            _ => panic!("unexpected sqlite_master type {kind}"),
        };
        conn.execute_raw(Statement::from_string(backend, statement))
            .await
            .unwrap_or_else(|error| panic!("drop {name}: {error}"));
    }
}

async fn capture_table_count(conn: &impl ConnectionTrait) -> i64 {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT COUNT(*) AS n FROM sqlite_master \
         WHERE type = 'table' AND name LIKE 'agent\\_%' ESCAPE '\\'"
            .to_string(),
    ))
    .await
    .expect("count capture tables")
    .expect("count row")
    .try_get_by::<i64, _>("n")
    .expect("decode capture table count")
}

#[tokio::test]
async fn capture_store_reader_commands_read_capture_database() {
    let repo = tempfile::tempdir().expect("reader repository");
    crate::utils::test::setup_with_new_libra_in(repo.path()).await;
    let _cwd = ChangeDirGuard::new(repo.path());
    let storage = repo.path().join(".libra");
    let database = storage.join(crate::utils::util::DATABASE);
    let repo_conn = crate::internal::db::open_repository_with_busy_timeout(
        database.to_str().expect("utf-8 database path"),
        Duration::from_secs(30),
    )
    .await
    .expect("open repository database");
    let schema_before = repo_conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT MAX(version) AS version FROM schema_versions".to_string(),
        ))
        .await
        .expect("schema version")
        .expect("schema row")
        .try_get_by::<Option<i64>, _>("version")
        .expect("decode schema version");
    drop_repository_capture_tables(&repo_conn).await;
    assert_eq!(capture_table_count(&repo_conn).await, 0);

    let capture = crate::internal::ai::capture::store::open(&storage, Duration::from_secs(30))
        .await
        .expect("open capture database after the repository tables are gone");
    capture
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
             working_dir, metadata_json, redaction_report, started_at, last_event_at) \
             VALUES ('cq03-reader', 'claude_code', 'cq03-reader', 'active', '/work', '{}', '{}', 5, 6)"
                .to_string(),
        ))
        .await
        .expect("insert capture-only session");
    capture
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid, \
             metadata_blob_oid, traces_commit, created_at) VALUES \
             ('cq03-checkpoint', 'cq03-reader', 'committed', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', \
              'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'cccccccccccccccccccccccccccccccccccccccc', 7)"
                .to_string(),
        ))
        .await
        .expect("insert capture-only checkpoint");
    crate::internal::ai::checkpoint_reader::load_checkpoint_catalog(
        &storage,
        "cq03-checkpoint",
        &crate::internal::ai::checkpoint_reader::CheckpointReadBudget::new(
            std::time::Instant::now() + Duration::from_secs(5),
            std::sync::Arc::new(|| false),
        ),
    )
    .await
    .expect("checkpoint catalog reader sees the capture-only row");

    let output = json_output();
    let mut status_out = gag::BufferRedirect::stdout().expect("capture status stdout");
    super::status::execute_safe(StatusArgs {}, &output)
        .await
        .expect("status reads the capture database");
    let mut status = String::new();
    std::io::Read::read_to_string(&mut status_out, &mut status).expect("read status");
    drop(status_out);
    assert!(
        status.contains("\"sessions_active\":1"),
        "status must count the capture-only session: {status}"
    );

    let mut session_out = gag::BufferRedirect::stdout().expect("capture session stdout");
    super::session::execute_safe(
        SessionSubcommand::List(SessionListArgs {
            agent: None,
            state: None,
            limit: None,
            cursor: None,
        }),
        &output,
    )
    .await
    .expect("session list reads the capture database");
    let mut sessions = String::new();
    std::io::Read::read_to_string(&mut session_out, &mut sessions).expect("read sessions");
    drop(session_out);
    assert!(
        sessions.contains("cq03-reader"),
        "session list must show the capture-only row: {sessions}"
    );

    let mut checkpoint_out = gag::BufferRedirect::stdout().expect("capture checkpoint stdout");
    super::checkpoint::execute_safe(
        CheckpointSubcommand::List(CheckpointListArgs {
            session: None,
            limit: None,
            cursor: None,
        }),
        &output,
    )
    .await
    .expect("checkpoint list reads the capture database");
    let mut checkpoints = String::new();
    std::io::Read::read_to_string(&mut checkpoint_out, &mut checkpoints).expect("read checkpoints");
    drop(checkpoint_out);
    assert!(
        checkpoints.contains("cq03-checkpoint"),
        "checkpoint list must show the capture-only row: {checkpoints}"
    );

    let mut doctor_out = gag::BufferRedirect::stdout().expect("capture doctor stdout");
    super::doctor::execute_safe(
        DoctorArgs {
            repair: false,
            commit_link_page: 1,
            codex_legacy_after: None,
            reset_commit_link_journal: false,
            acknowledge_commit_link_loss: false,
            codex_index_backup: None,
            codex_index_page: None,
        },
        &output,
    )
    .await
    .expect("doctor reads capture counts");
    let mut doctor = String::new();
    std::io::Read::read_to_string(&mut doctor_out, &mut doctor).expect("read doctor");
    drop(doctor_out);
    assert!(
        doctor.contains("\"active_sessions\":1"),
        "doctor must count the capture-only session: {doctor}"
    );

    let mut graph_out = gag::BufferRedirect::stdout().expect("capture graph stdout");
    super::graph::execute_safe(
        GraphArgs {
            session: "cq03-reader".to_string(),
            repo: Some(repo.path().to_path_buf()),
        },
        &output,
    )
    .await
    .expect("graph reads the capture database");
    let mut graph = String::new();
    std::io::Read::read_to_string(&mut graph_out, &mut graph).expect("read graph");
    drop(graph_out);
    assert!(
        graph.contains("cq03-reader"),
        "graph must project the capture-only session: {graph}"
    );

    super::skill::execute_safe(
        SkillSubcommand::Search(SkillSearchArgs {
            skill: None,
            provider: None,
            session: Some("cq03-reader".to_string()),
            since: None,
            until: None,
            limit: Some(1),
            cursor: None,
        }),
        &output,
    )
    .await
    .expect("skill search reads the capture checkpoint catalog");

    super::clean::execute_safe(
        CleanArgs {
            all: true,
            gc: false,
            retention_days: None,
            dry_run: false,
        },
        &output,
    )
    .await
    .expect("clean reads the capture catalog");

    let schema_after = repo_conn
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT MAX(version) AS version FROM schema_versions".to_string(),
        ))
        .await
        .expect("schema version after readers")
        .expect("schema row")
        .try_get_by::<Option<i64>, _>("version")
        .expect("decode schema version");
    assert_eq!(schema_before, schema_after);
    assert_eq!(
        capture_table_count(&repo_conn).await,
        0,
        "command readers must not recreate capture tables in libra.db"
    );
    let still_there = capture
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'cq03-reader'".to_string(),
        ))
        .await
        .expect("re-read capture session")
        .expect("count row")
        .try_get_by::<i64, _>("n")
        .expect("decode session count");
    assert_eq!(still_there, 1);
}

#[tokio::test]
async fn capture_store_reader_repository_commands_stay_on_repository_database() {
    let repo = tempfile::tempdir().expect("repository-reader repository");
    crate::utils::test::setup_with_new_libra_in(repo.path()).await;
    let _cwd = ChangeDirGuard::new(repo.path());
    let storage = repo.path().join(".libra");
    let opened = super::mega2_stream::open_repository_at(repo.path())
        .await
        .expect("mega2 opens the repository database");
    let has_config = opened
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT 1 AS n FROM sqlite_master WHERE type = 'table' AND name = 'config_kv'"
                .to_string(),
        ))
        .await
        .expect("probe config_kv")
        .is_some();
    assert!(has_config, "mega2 must open the repository database");
    let capture_path = crate::internal::ai::capture::store::capture_database_path(&storage);
    assert!(
        !capture_path.exists(),
        "opening the repository database must not create capture.db"
    );

    super::workspace::execute_safe(
        WorkspaceSubcommand::List(WorkspaceListArgs {
            limit: Some(1),
            cursor: None,
            states: Vec::new(),
        }),
        &json_output(),
    )
    .await
    .expect("workspace list reads repository records");
    assert!(
        !capture_path.exists(),
        "workspace list must not open the capture database"
    );
}
