//! A0-07: `libra agent skill search` reads skill events out of checkpoint
//! metadata blobs and filters them by skill / provider / session / time.
//!
//! The extraction+embedding path is covered by
//! `agent_transcript_intelligence_test`; here we seed real metadata blobs
//! (`extraction.skill_events`) plus their `agent_checkpoint` rows and exercise
//! the read/search command surface end to end through the binary.

use std::{path::Path, time::Duration};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use serial_test::serial;

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
};

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect repository database")
}

/// A single `SkillEvent` JSON with the given fields (E7 wire shape).
fn skill_event_json(turn: &str, skill: &str, agent: &str, ts: &str) -> String {
    format!(
        r#"{{"id":"{turn}:{skill}","event_type":"prompt_invocation",
            "skill":{{"name":"{skill}"}},
            "source":{{"agent":"{agent}","signal":"input_slash_command","confidence":1.0}},
            "turn_id":"{turn}","timestamp":"{ts}","native":false,"collapse":false}}"#
    )
}

/// Write a checkpoint metadata blob carrying `skill_events` and insert its
/// `agent_session` + `agent_checkpoint` rows. Returns nothing; the command
/// reads them back through the object store.
async fn seed_skill_checkpoint(
    conn: &DatabaseConnection,
    repo: &Path,
    session_id: &str,
    provider: &str,
    checkpoint_id: &str,
    created_at: i64,
    events_json: &[String],
) {
    let metadata = format!(
        r#"{{"schema_version":2,"extraction":{{"schema_version":1,"skill_events":[{}]}}}}"#,
        events_json.join(",")
    );
    let oid =
        libra::utils::object::write_git_object(&repo.join(".libra"), "blob", metadata.as_bytes())
            .expect("write metadata blob")
            .to_string();

    // agent_kind is the DB enum form (underscores), NOT the CLI slug — a
    // CHECK constraint rejects the hyphenated slug, and INSERT OR IGNORE would
    // then silently drop the row and break the checkpoint FK.
    let agent_kind_db = provider.replace('-', "_");
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT OR IGNORE INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, ?, ?, 'stopped', '/tmp/libra-skill-test', '{}', '{}', 10, 20, 30)",
        vec![
            Value::from(session_id),
            Value::from(agent_kind_db),
            Value::from(format!("provider-{session_id}")),
        ],
    ))
    .await
    .expect("insert agent_session");

    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, 'committed', NULL,
                   '2222222222222222222222222222222222222222', ?,
                   '3333333333333333333333333333333333333333', ?)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(oid),
            Value::from(created_at),
        ],
    ))
    .await
    .expect("insert agent_checkpoint");
}

#[tokio::test]
#[serial]
async fn agent_skill_search() {
    let repo_guard = create_committed_repo_via_cli();
    let repo = repo_guard.path().to_path_buf();
    let conn = connect_repo_db(&repo).await;

    // Two providers, two sessions, three skill events.
    seed_skill_checkpoint(
        &conn,
        &repo,
        "sess-claude",
        "claude-code",
        "ckpt-claude",
        100,
        &[
            skill_event_json("t1", "/review", "claude-code", "2026-07-09T01:00:00Z"),
            skill_event_json("t2", "/simplify", "claude-code", "2026-07-09T02:00:00Z"),
        ],
    )
    .await;
    seed_skill_checkpoint(
        &conn,
        &repo,
        "sess-codex",
        "codex",
        "ckpt-codex",
        200,
        &[skill_event_json(
            "t3",
            "/review",
            "codex",
            "2026-07-09T03:00:00Z",
        )],
    )
    .await;

    // Unfiltered search returns all three, newest first.
    let out = run_libra_command(&["agent", "skill", "search", "--json"], &repo);
    assert_cli_success(&out, "agent skill search");
    let json = parse_json_stdout(&out);
    let events = json["data"]["skill_events"]
        .as_array()
        .expect("skill_events array");
    assert_eq!(events.len(), 3, "all seeded events: {json}");
    assert_eq!(json["data"]["schema_version"], 1);
    // Newest first (the codex /review at 03:00 leads).
    assert_eq!(events[0]["event"]["skill"]["name"], "/review");
    assert_eq!(events[0]["provider"], "codex");

    // Filter by skill name.
    let out = run_libra_command(
        &["agent", "skill", "search", "--skill", "/review", "--json"],
        &repo,
    );
    assert_cli_success(&out, "search --skill");
    let json = parse_json_stdout(&out);
    let events = json["data"]["skill_events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "two /review events: {json}");
    assert!(
        events
            .iter()
            .all(|e| e["event"]["skill"]["name"] == "/review")
    );

    // Filter by provider.
    let out = run_libra_command(
        &["agent", "skill", "search", "--provider", "codex", "--json"],
        &repo,
    );
    assert_cli_success(&out, "search --provider");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["skill_events"].as_array().unwrap().len(), 1);

    // Filter by session.
    let out = run_libra_command(
        &[
            "agent",
            "skill",
            "search",
            "--session",
            "sess-claude",
            "--json",
        ],
        &repo,
    );
    assert_cli_success(&out, "search --session");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["skill_events"].as_array().unwrap().len(), 2);

    // Time-range filter (only the 03:00 codex event).
    let out = run_libra_command(
        &[
            "agent",
            "skill",
            "search",
            "--since",
            "2026-07-09T02:30:00Z",
            "--json",
        ],
        &repo,
    );
    assert_cli_success(&out, "search --since");
    let json = parse_json_stdout(&out);
    let events = json["data"]["skill_events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["provider"], "codex");

    // A malformed RFC3339 bound is a usage error, never a silently-unbounded
    // search that returns everything.
    let out = run_libra_command(
        &[
            "agent",
            "skill",
            "search",
            "--since",
            "not-a-date",
            "--json",
        ],
        &repo,
    );
    assert!(
        !out.status.success(),
        "malformed --since must fail, not silently return all events"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("RFC3339"),
        "the usage error names the RFC3339 requirement: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The curated registry surface lists discoverable skills per agent.
    let out = run_libra_command(&["agent", "skill", "registry", "--json"], &repo);
    assert_cli_success(&out, "skill registry");
    let json = parse_json_stdout(&out);
    let skills = json["data"]["skills"].as_array().expect("skills array");
    assert!(
        skills
            .iter()
            .any(|s| s["provider"] == "claude-code" && s["name"] == "/security-review"),
        "registry lists curated skills: {json}"
    );
}
