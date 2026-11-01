//! GK-01/GK-05: Repository `agent_kind=grok` round-trips with the supported Grok wave.

use std::path::Path;

use libra::{
    command::agent::{AgentArgs, AgentSubcommand, DisableArgs, EnableArgs, execute_safe},
    internal::{
        ai::{
            capture::deadline_provider_extraction_summary,
            observed_agents::{
                AgentKind, AgentStability, agent_for, extract::skill_registry_for, is_preview,
                registration_for, supported_slugs,
            },
        },
        db::{
            DatabaseRole,
            migration::{MigrationRunner, builtin_migrations, builtin_runner},
            schema::upgrade_database_schema_for_role,
        },
    },
    utils::output::{ColorChoice, OutputConfig, ProgressMode, ProgressPreference},
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};

const PREVIOUS_TIP: i64 = 2026092902;
const GROK_TIP: i64 = 2026101901;

#[tokio::test]
async fn grok_agent_kind_round_trip_and_check() {
    let root = tempfile::tempdir().expect("temp root");
    let global = root.path().join("global.db");
    let system = root.path().join("system.db");
    let derived = root.path().join("derived.db");
    seed_untouched_db(&global, "global_marker").await;
    seed_untouched_db(&system, "system_marker").await;
    seed_untouched_db(&derived, "derived_marker").await;
    let global_bytes = std::fs::read(&global).expect("read global");
    let system_bytes = std::fs::read(&system).expect("read system");
    let derived_bytes = std::fs::read(&derived).expect("read derived");

    let repo = root.path().join("repo.db");
    std::fs::File::create(&repo).expect("create repo db");
    let conn = connect(&repo).await;
    let runner = builtin_runner().expect("builtin runner");
    runner
        .run_pending_up_to(&conn, PREVIOUS_TIP)
        .await
        .expect("migrate to the pre-grok tip");
    exec(
        &conn,
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)",
    )
    .await;
    for kind in [
        "claude_code",
        "cursor",
        "codex",
        "gemini",
        "opencode",
        "copilot",
        "factory_ai",
    ] {
        insert_session(&conn, &format!("sess-{kind}"), kind).await;
    }
    insert_checkpoint(&conn, "sess-claude_code").await;
    let before_sessions = count(&conn, "SELECT COUNT(*) FROM agent_session").await;
    let before_checkpoints = count(&conn, "SELECT COUNT(*) FROM agent_checkpoint").await;
    assert_eq!(before_sessions, 7);
    assert_eq!(before_checkpoints, 1);

    let applied = runner
        .run_pending_up_to(&conn, GROK_TIP)
        .await
        .expect("apply grok CHECK migration");
    assert!(
        applied.contains(&2026101901),
        "grok migration did not apply: {applied:?}"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM agent_session").await,
        before_sessions
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM agent_checkpoint").await,
        before_checkpoints
    );
    assert_eq!(std::fs::read(&global).expect("reread global"), global_bytes);
    assert_eq!(std::fs::read(&system).expect("reread system"), system_bytes);
    assert_eq!(
        std::fs::read(&derived).expect("reread derived"),
        derived_bytes
    );

    insert_session(&conn, "sess-grok", "grok").await;
    let kinds = text_column(
        &conn,
        "SELECT agent_kind FROM agent_session ORDER BY agent_kind",
    )
    .await;
    assert_eq!(
        kinds,
        vec![
            "claude_code",
            "codex",
            "copilot",
            "cursor",
            "factory_ai",
            "gemini",
            "grok",
            "opencode",
        ]
    );

    let refused = runner.rollback_to(&conn, PREVIOUS_TIP).await;
    let message = refused.expect_err("down must refuse while a grok row exists");
    assert!(
        message.to_string().contains("were kept"),
        "down error should keep the grok rows: {message}"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM agent_session WHERE agent_kind = 'grok'"
        )
        .await,
        1,
        "refused down must keep the grok row"
    );
    assert_eq!(
        runner.current_version(&conn).await.expect("version"),
        Some(2026101901)
    );

    exec(&conn, "DELETE FROM agent_session WHERE agent_kind = 'grok'").await;
    let rolled = runner
        .rollback_to(&conn, PREVIOUS_TIP)
        .await
        .expect("down with zero grok rows");
    assert_eq!(rolled, vec![2026101901]);
    let rejected = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES (?, 'grok', 'p-grok', 'stopped', '/work', '{}', '{}', 1, 1)",
            ["sess-grok-again".into()],
        ))
        .await;
    assert!(
        rejected.is_err(),
        "old CHECK must reject grok after rollback"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM agent_session WHERE session_id = 'sess-claude_code'"
        )
        .await,
        1
    );

    runner
        .run_pending_up_to(&conn, GROK_TIP)
        .await
        .expect("re-apply grok migration");
    insert_session(&conn, "sess-grok", "grok").await;

    assert_eq!(AgentKind::Grok.as_db_str(), "grok");
    assert_eq!(AgentKind::Grok.as_cli_slug(), "grok");
    assert_eq!(AgentKind::from_db_str("grok"), Some(AgentKind::Grok));
    assert_eq!(AgentKind::from_cli_slug("grok"), Some(AgentKind::Grok));
    assert!(!is_preview(AgentKind::Grok));
    let agent = agent_for(AgentKind::Grok);
    assert_eq!(agent.provider_kind(), AgentKind::Grok);
    assert_eq!(agent.stability(), AgentStability::Stable);
    assert!(agent.as_hooks().is_some());
    assert_eq!(agent.protected_dirs(), &[".grok"]);
    let row = registration_for(AgentKind::Grok);
    assert!(row.supported);
    assert!(row.hook_installable);
    assert!(!row.launchable_review);
    assert!(!row.launchable_investigate);
    assert_eq!(
        supported_slugs(),
        vec!["claude-code", "codex", "opencode", "grok", "hermes"]
    );

    let summary = deadline_provider_extraction_summary(AgentKind::Grok, b"thought-canary");
    assert!(summary.partial);
    assert!(summary.prompts.is_empty());
    assert!(
        summary
            .warnings
            .iter()
            .all(|warning| !warning.contains("thought-canary"))
    );
    assert!(skill_registry_for(AgentKind::Grok).is_empty());

    let output = OutputConfig {
        json_format: None,
        color: ColorChoice::Never,
        pager: false,
        quiet: true,
        exit_code_on_warning: false,
        progress: ProgressMode::None,
        progress_preference: ProgressPreference::None,
    };
    let home = root.path().join("grok-home");
    let previous_home = std::env::var_os("GROK_HOME");
    // SAFETY: this test owns the process-local GROK_HOME override and restores it
    // before returning. The enable path must not touch a real Grok home.
    unsafe { std::env::set_var("GROK_HOME", &home) };
    let enable = execute_safe(
        AgentArgs {
            command: AgentSubcommand::Enable(EnableArgs {
                agents: vec!["grok".to_string()],
                hook_timeout: None,
            }),
        },
        &output,
        None,
    )
    .await;
    let enable_text = format!("{enable:?}");
    assert!(enable.is_ok(), "{enable_text}");
    let installed = home.join("hooks").join("libra-grok-capture.json");
    let installed_text = std::fs::read_to_string(&installed).expect("marked hook file");
    assert!(installed_text.contains("libra capture"));
    assert!(installed_text.contains("agent hooks grok"));
    assert!(installed_text.starts_with('{') || installed_text.contains('/'));
    let disable = execute_safe(
        AgentArgs {
            command: AgentSubcommand::Disable(DisableArgs {
                agents: vec!["grok".to_string()],
            }),
        },
        &output,
        None,
    )
    .await;
    assert!(disable.is_ok(), "{disable:?}");
    assert!(!installed.exists(), "disable removes the marked hook file");
    unsafe {
        match previous_home {
            Some(value) => std::env::set_var("GROK_HOME", value),
            None => std::env::remove_var("GROK_HOME"),
        }
    }
    assert!(!root.path().join(".libra").exists());
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM agent_session").await,
        8,
        "enable must not write agent_session"
    );

    let sessions_before = digest(
        &conn,
        "SELECT session_id || '|' || agent_kind FROM agent_session ORDER BY session_id",
    )
    .await;
    let receipts_before = digest(
        &conn,
        "SELECT CAST(version AS TEXT) || '|' || name FROM schema_versions ORDER BY version",
    )
    .await;
    exec(
        &conn,
        "INSERT INTO schema_versions (version, name, applied_at) VALUES (2099010101, 'future-reader', '2000-01-01T00:00:00Z')",
    )
    .await;
    drop(conn);
    let refused_open = upgrade_database_schema_for_role(&repo, DatabaseRole::Repository).await;
    let open_error = refused_open.expect_err("a reader older than the file must refuse");
    assert!(open_error.to_string().contains("newer"), "{open_error}");
    let conn = connect(&repo).await;
    assert_eq!(
        digest(
            &conn,
            "SELECT session_id || '|' || agent_kind FROM agent_session ORDER BY session_id",
        )
        .await,
        sessions_before
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM schema_versions WHERE version = 2099010101 AND name = 'future-reader'"
        )
        .await,
        1,
        "the refused open must leave the future receipt untouched"
    );
    let _ = receipts_before;
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM agent_session WHERE agent_kind = 'grok'"
        )
        .await,
        1
    );
    let _ = MigrationRunner::new();
    let _ = builtin_migrations();
}

async fn seed_untouched_db(path: &Path, marker: &str) {
    std::fs::File::create(path).expect("create side db");
    let conn = connect(path).await;
    exec(&conn, "CREATE TABLE marker (name TEXT NOT NULL)").await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO marker (name) VALUES (?)",
        [marker.into()],
    ))
    .await
    .expect("insert marker");
    conn.close().await.expect("close side db");
}

async fn connect(path: &Path) -> DatabaseConnection {
    let url = format!("sqlite://{}", path.display());
    let mut options = sea_orm::ConnectOptions::new(url);
    options.sqlx_logging(false);
    Database::connect(options).await.expect("connect")
}

async fn insert_session(conn: &DatabaseConnection, session_id: &str, kind: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES (?, ?, ?, 'stopped', '/work', '{}', '{}', 1, 1)",
        [session_id.into(), kind.into(), format!("provider-{session_id}").into()],
    ))
    .await
    .unwrap_or_else(|error| panic!("insert {kind}: {error}"));
}

async fn insert_checkpoint(conn: &DatabaseConnection, session_id: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES ('cp-1', ?, 'temporary', 'parent', 'tree', 'meta', 'traces', 1)",
        [session_id.into()],
    ))
    .await
    .unwrap_or_else(|error| panic!("insert checkpoint: {error}"));
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn
        .query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("count")
        .expect("count row");
    row.try_get_by_index(0).expect("count value")
}

async fn text_column(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    let rows = conn
        .query_all_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("text query");
    rows.into_iter()
        .map(|row| row.try_get_by_index(0).expect("text"))
        .collect()
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("exec {sql}: {error}"));
}

async fn digest(conn: &DatabaseConnection, sql: &str) -> String {
    let rows = text_column(conn, sql).await;
    let mut hasher = Sha256::new();
    for row in rows {
        hasher.update(row.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}
