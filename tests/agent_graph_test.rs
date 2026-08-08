//! plan-20260713 M6 / DR-07 capture-graph contract.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Output},
};

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::Value;
use tempfile::TempDir;

const SESSION_ID: &str = "codex__graph-fixture";
const CLAUDE_SUBAGENT_SESSION_ID: &str = "claude__subagent-fixture";
const LEGACY_SESSION_ID: &str = "claude__legacy-fixture";
const ERASED_SESSION_ID: &str = "opencode__erased-fixture";
const INFLIGHT_SESSION_ID: &str = "codex__inflight-fixture";

struct GraphRepo {
    _tmp: TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl GraphRepo {
    async fn init() -> Self {
        let tmp = TempDir::new().expect("create graph test tempdir");
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::create_dir_all(&home).expect("create home dir");
        let fixture = Self {
            _tmp: tmp,
            repo,
            home,
        };
        let output = fixture.run(&["init"]);
        assert!(output.status.success(), "init: {}", describe(&output));
        fixture.seed().await;
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env_remove("CODEX_HOME");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run libra")
    }

    async fn connection(&self) -> DatabaseConnection {
        let url = format!(
            "sqlite://{}?mode=rwc",
            self.repo.join(".libra/libra.db").display()
        );
        Database::connect(url).await.expect("open graph fixture db")
    }

    async fn seed(&self) {
        let connection = self.connection().await;
        connection
            .execute_unprepared(
                r#"
INSERT INTO agent_session (
    session_id, agent_kind, provider_session_id, state, working_dir,
    metadata_json, redaction_report, started_at, last_event_at, schema_version
) VALUES
    ('codex__graph-fixture', 'codex', 'graph-fixture', 'stopped',
     '/private/SUPER_SECRET/repo',
     '{"transcript_path":"/private/SUPER_SECRET/transcript.jsonl"}',
     '{"raw":"SUPER_SECRET"}', 100, 900, 1),
    ('claude__legacy-fixture', 'claude_code', 'legacy-fixture', 'stopped',
     '/private/LEGACY_SECRET/repo',
     '{"transcript_path":"/private/LEGACY_SECRET/session.jsonl"}',
     '{"raw":"LEGACY_SECRET"}', 50, 60, 1),
    ('claude__subagent-fixture', 'claude_code', 'subagent-fixture', 'stopped',
     '/private/CLAUDE_SECRET/repo',
     '{"transcript_path":"/private/CLAUDE_SECRET/session.jsonl"}',
     '{"raw":"CLAUDE_SECRET"}', 700, 900, 1),
    ('codex__inflight-fixture', 'codex', 'inflight-fixture', 'active',
     '/private/INFLIGHT_SECRET/repo',
     '{"transcript_path":"/private/INFLIGHT_SECRET/session.jsonl"}',
     '{"raw":"INFLIGHT_SECRET"}', 1000, 1100, 1);

INSERT INTO agent_checkpoint (
    checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
    tree_oid, metadata_blob_oid, traces_commit, description, created_at
) VALUES
    ('checkpoint-shared', 'codex__graph-fixture', NULL, 'committed',
     'PARENT_SECRET', 'TREE_SECRET', 'META_SECRET', 'trace-shared',
     'SUPER_SECRET shared description', 200),
    ('checkpoint-current', 'codex__graph-fixture', NULL, 'committed',
     'PARENT_SECRET', 'TREE_SECRET', 'META_SECRET', 'trace-current',
     '/private/SUPER_SECRET/current description', 300),
    ('checkpoint-boundary', 'codex__graph-fixture', 'checkpoint-current', 'subagent',
     'PARENT_SECRET', 'TREE_SECRET', 'META_SECRET', 'trace-boundary',
     'SUPER_SECRET boundary', 400),
    ('checkpoint-subagent-resolved', 'codex__graph-fixture', NULL, 'subagent',
     'PARENT_SECRET', 'TREE_SECRET', 'META_SECRET', 'trace-sub-resolved',
     'SUPER_SECRET resolved content', 500),
    ('checkpoint-subagent-unresolved', 'codex__graph-fixture', NULL, 'subagent',
     'PARENT_SECRET', 'TREE_SECRET', 'META_SECRET', 'trace-sub-unresolved',
     'SUPER_SECRET unresolved content', 600),
    ('checkpoint-legacy', 'claude__legacy-fixture', NULL, 'committed',
     'LEGACY_PARENT_SECRET', 'LEGACY_TREE_SECRET', 'LEGACY_META_SECRET',
     'trace-legacy', '/private/LEGACY_SECRET/description', 55),
    ('checkpoint-claude-parent', 'claude__subagent-fixture', NULL, 'committed',
     'CLAUDE_PARENT_SECRET', 'CLAUDE_TREE_SECRET', 'CLAUDE_META_SECRET',
     'trace-claude-parent', '/private/CLAUDE_SECRET/parent', 750),
    ('checkpoint-claude-subagent', 'claude__subagent-fixture', NULL, 'subagent',
     'CLAUDE_PARENT_SECRET', 'CLAUDE_TREE_SECRET', 'CLAUDE_META_SECRET',
     'trace-claude-subagent', '/private/CLAUDE_SECRET/subagent', 800),
    ('checkpoint-inflight-current', 'codex__inflight-fixture', NULL, 'committed',
     'INFLIGHT_PARENT_SECRET', 'INFLIGHT_TREE_SECRET', 'INFLIGHT_META_SECRET',
     'trace-inflight-current', '/private/INFLIGHT_SECRET/current', 1050);

INSERT INTO agent_coverage_claim (
    session_id, logical_turn_key, coverage_schema_version, coverage_digest,
    completeness, revision, state, checkpoint_id, traces_commit,
    source_channel, created_at, updated_at
) VALUES
    ('codex__graph-fixture', 'turn:alpha', 1, 'SUPER_SECRET_DIGEST_A2',
     'complete', 2, 'catalog_committed', 'checkpoint-current', 'trace-current',
     'live', 200, 300),
    ('codex__graph-fixture', 'turn:beta', 1, 'SUPER_SECRET_DIGEST_B1',
     'complete', 1, 'catalog_committed', 'checkpoint-shared', 'trace-shared',
     'import', 200, 200),
    ('claude__subagent-fixture', 'turn:claude', 1, 'CLAUDE_SECRET_DIGEST',
     'complete', 1, 'catalog_committed', 'checkpoint-claude-parent',
     'trace-claude-parent', 'live', 750, 750),
    ('codex__inflight-fixture', 'turn:inflight', 1, 'INFLIGHT_INCOMING_DIGEST',
     'complete', 1, 'reserved_import', 'checkpoint-inflight-current',
     'trace-inflight-current', 'import', 1050, 1100);

INSERT INTO agent_coverage_revision (
    session_id, logical_turn_key, coverage_schema_version, revision,
    checkpoint_id, coverage_digest, completeness, source_channel, created_at
) VALUES
    ('codex__graph-fixture', 'turn:alpha', 1, 1, 'checkpoint-shared',
     'SUPER_SECRET_DIGEST_A1', 'incomplete', 'live', 200),
    ('codex__graph-fixture', 'turn:alpha', 1, 2, 'checkpoint-current',
     'SUPER_SECRET_DIGEST_A2', 'complete', 'live', 300),
    ('codex__graph-fixture', 'turn:beta', 1, 1, 'checkpoint-shared',
     'SUPER_SECRET_DIGEST_B1', 'complete', 'import', 200),
    ('claude__subagent-fixture', 'turn:claude', 1, 1, 'checkpoint-claude-parent',
     'CLAUDE_SECRET_DIGEST', 'complete', 'live', 750),
    ('codex__inflight-fixture', 'turn:inflight', 1, 1,
     'checkpoint-inflight-current', 'INFLIGHT_COMMITTED_DIGEST', 'incomplete',
     'live', 1050);

INSERT INTO agent_subagent_link (
    content_checkpoint_id, parent_session_id, link_state,
    boundary_checkpoint_id, stable_subagent_id, created_at, updated_at
) VALUES
    ('checkpoint-subagent-resolved', 'codex__graph-fixture', 'resolved',
     'checkpoint-boundary', 'stable-subagent-id', 500, 500),
    ('checkpoint-subagent-unresolved', 'codex__graph-fixture', 'unresolved',
     NULL, NULL, 600, 600),
    ('checkpoint-claude-subagent', 'claude__subagent-fixture', 'unresolved',
     NULL, NULL, 800, 800);

INSERT INTO agent_import_tombstone (
    tombstone_id, agent_kind, provider_session_id, erased_session_id,
    source_fingerprint, erased_at
) VALUES (
    'tombstone-graph-fixture', 'opencode', 'erased-fixture',
    'opencode__erased-fixture', 'SUPER_SECRET_FINGERPRINT', 700
);
"#,
            )
            .await
            .expect("seed graph fixture");
    }

    async fn capture_snapshot(&self) -> BTreeMap<String, String> {
        let connection = self.connection().await;
        let tables = [
            "agent_session",
            "agent_checkpoint",
            "agent_coverage_claim",
            "agent_coverage_revision",
            "agent_import_tombstone",
            "agent_import_identity",
            "agent_export_job",
            "agent_subagent_content_claim",
            "agent_subagent_content_revision",
            "agent_subagent_link",
        ];
        let mut snapshot = BTreeMap::new();
        for table in tables {
            let columns = connection
                .query_all_raw(Statement::from_string(
                    connection.get_database_backend(),
                    format!("PRAGMA table_info({table})"),
                ))
                .await
                .expect("read fixture table columns")
                .into_iter()
                .map(|row| {
                    let name: String = row.try_get_by("name").expect("column name");
                    name
                })
                .collect::<Vec<_>>();
            let signature = columns
                .iter()
                .map(|column| format!("quote(\"{column}\")"))
                .collect::<Vec<_>>()
                .join(" || char(31) || ");
            let sql = format!(
                "SELECT COALESCE(group_concat(row_signature, char(30)), '') AS snapshot \
                 FROM (SELECT {signature} AS row_signature FROM {table} ORDER BY row_signature)"
            );
            let row = connection
                .query_one_raw(Statement::from_string(
                    connection.get_database_backend(),
                    sql,
                ))
                .await
                .expect("snapshot fixture table")
                .expect("snapshot aggregate row");
            snapshot.insert(
                table.to_string(),
                row.try_get_by("snapshot").expect("snapshot string"),
            );
        }
        snapshot
    }
}

fn describe(output: &Output) -> String {
    format!(
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn json_output(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        describe(output)
    );
    serde_json::from_slice(&output.stdout).expect("parse graph JSON envelope")
}

#[tokio::test]
async fn agent_graph_renders_session_turn_revisions_and_preserves_shared_checkpoint() {
    let fixture = GraphRepo::init().await;
    let output = fixture.run(&["--json", "agent", "graph", SESSION_ID]);
    let payload = json_output(&output);
    assert_eq!(payload["command"], "agent_graph");
    assert_eq!(payload["data"]["schema_version"], 1);
    assert_eq!(payload["data"]["state"], "present");

    let turns = payload["data"]["turns"].as_array().expect("turn array");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["logical_turn_key"], "turn:alpha");
    assert_eq!(turns[0]["ordinal"], 0);
    assert_eq!(turns[0]["current_revision"], 2);
    assert_eq!(turns[0]["revisions"].as_array().map(Vec::len), Some(2));
    assert_eq!(turns[1]["logical_turn_key"], "turn:beta");
    assert_eq!(turns[1]["ordinal"], 1);

    let alpha_history = turns[0]["revisions"].as_array().expect("alpha revisions");
    assert_eq!(alpha_history[0]["checkpoint_id"], "checkpoint-shared");
    assert_eq!(turns[1]["checkpoint_id"], "checkpoint-shared");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("superseded"),
        "a checkpoint shared by two turns must never be hidden as superseded"
    );
}

#[tokio::test]
async fn agent_graph_subagent_link_states_are_explicit() {
    let fixture = GraphRepo::init().await;
    let codex = json_output(&fixture.run(&["--json", "agent", "graph", SESSION_ID]));
    let subagents = &codex["data"]["subagents"];
    assert_eq!(subagents["available"], true);
    let nodes = subagents["nodes"].as_array().expect("subagent nodes");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["link_state"], "resolved");
    assert_eq!(nodes[0]["boundary_checkpoint_id"], "checkpoint-boundary");
    assert_eq!(nodes[1]["link_state"], "unresolved");
    assert!(nodes[1]["boundary_checkpoint_id"].is_null());

    let claude =
        json_output(&fixture.run(&["--json", "agent", "graph", CLAUDE_SUBAGENT_SESSION_ID]));
    assert_eq!(claude["data"]["session"]["agent_kind"], "claude_code");
    let claude_nodes = claude["data"]["subagents"]["nodes"]
        .as_array()
        .expect("Claude subagent nodes");
    assert_eq!(claude_nodes.len(), 1);
    assert_eq!(claude_nodes[0]["link_state"], "unresolved");
    assert!(claude_nodes[0]["boundary_checkpoint_id"].is_null());
}

#[tokio::test]
async fn agent_graph_legacy_unindexed_session_is_readonly() {
    let fixture = GraphRepo::init().await;
    let before = fixture.capture_snapshot().await;
    let payload = json_output(&fixture.run(&["--json", "agent", "graph", LEGACY_SESSION_ID]));
    let after = fixture.capture_snapshot().await;
    assert_eq!(
        before, after,
        "legacy graph must not backfill capture tables"
    );
    let turns = payload["data"]["turns"].as_array().expect("legacy turns");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["coverage_state"], "unindexed");
    assert_eq!(turns[0]["logical_turn_key"], "checkpoint:checkpoint-legacy");
    assert!(turns[0]["coverage_schema_version"].is_null());
    assert!(turns[0]["current_revision"].is_null());
    assert_eq!(turns[0]["revisions"], Value::Array(Vec::new()));
}

#[tokio::test]
async fn agent_graph_inflight_upgrade_renders_last_committed_revision() {
    let fixture = GraphRepo::init().await;
    let payload = json_output(&fixture.run(&["--json", "agent", "graph", INFLIGHT_SESSION_ID]));
    let turn = &payload["data"]["turns"][0];
    assert_eq!(turn["current_revision"], 1);
    assert_eq!(turn["completeness"], "incomplete");
    assert_eq!(turn["source_channel"], "live");
    assert_eq!(turn["revisions"][0]["completeness"], "incomplete");
    assert_eq!(turn["revisions"][0]["source_channel"], "live");
}

#[tokio::test]
async fn agent_graph_json_and_machine_non_tty_never_launch_tui() {
    let fixture = GraphRepo::init().await;
    for flag in ["--json", "--machine"] {
        let output = fixture.run(&[flag, "agent", "graph", SESSION_ID]);
        let payload = json_output(&output);
        assert_eq!(payload["data"]["state"], "present");
    }
}

#[tokio::test]
async fn agent_graph_non_tty_without_json_refuses_before_tui() {
    let fixture = GraphRepo::init().await;
    let output = fixture.run(&["agent", "graph", SESSION_ID]);
    assert_eq!(output.status.code(), Some(129), "{}", describe(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"));
    assert!(stderr.contains("--json or --machine"));
    assert!(!stderr.contains("failed to run agent graph TUI"));
}

#[tokio::test]
async fn agent_graph_unknown_session_has_stable_error() {
    let fixture = GraphRepo::init().await;
    let output = fixture.run(&["--json", "agent", "graph", "unknown-session"]);
    assert_eq!(output.status.code(), Some(128), "{}", describe(&output));
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error envelope");
    assert_eq!(error["error_code"], "LBR-AGENT-021");
    assert_eq!(error["category"], "internal");
}

#[tokio::test]
async fn agent_graph_erased_session_shows_erased_without_resurrection() {
    let fixture = GraphRepo::init().await;
    let before = fixture.capture_snapshot().await;
    let payload = json_output(&fixture.run(&["--json", "agent", "graph", ERASED_SESSION_ID]));
    let after = fixture.capture_snapshot().await;
    assert_eq!(before, after, "erased graph must not recreate a session");
    assert_eq!(payload["data"]["state"], "erased");
    assert!(payload["data"]["session"].is_null());
    assert_eq!(payload["data"]["turns"], Value::Array(Vec::new()));
    assert_eq!(payload["data"]["subagents"]["available"], false);
    assert_eq!(payload["data"]["subagents"]["unavailable_reason"], "erased");
}

#[tokio::test]
async fn agent_graph_repo_flag_uses_target_repo_during_preflight() {
    let fixture = GraphRepo::init().await;
    let outside = fixture._tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("create outside dir");
    let output = fixture
        .command()
        .current_dir(&outside)
        .args([
            "--json",
            "agent",
            "graph",
            SESSION_ID,
            "--repo",
            fixture.repo.to_str().expect("UTF-8 fixture repo"),
        ])
        .output()
        .expect("run graph against explicit repo");
    let payload = json_output(&output);
    assert_eq!(payload["data"]["session"]["session_id"], SESSION_ID);
}

#[tokio::test]
async fn agent_graph_output_has_no_raw_transcript_absolute_path_or_free_text() {
    let fixture = GraphRepo::init().await;
    let output = fixture.run(&["--json", "agent", "graph", SESSION_ID]);
    let _ = json_output(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for forbidden in [
        "SUPER_SECRET",
        "/private/",
        "transcript_path",
        "description",
        "coverage_digest",
        "metadata_json",
        "redaction_report",
        "tree_oid",
        "traces_commit",
        "parent_commit",
    ] {
        assert!(
            !stdout.contains(forbidden),
            "agent graph leaked forbidden field/value `{forbidden}`: {stdout}"
        );
    }
}

#[tokio::test]
async fn agent_graph_json_schema_is_pinned_and_command_is_zero_mutation() {
    let fixture = GraphRepo::init().await;
    let before = fixture.capture_snapshot().await;
    let payload = json_output(&fixture.run(&["--json", "agent", "graph", SESSION_ID]));
    let after = fixture.capture_snapshot().await;
    assert_eq!(
        before, after,
        "agent graph mutated capture/import/export tables"
    );

    let data = payload["data"].as_object().expect("data object");
    assert_eq!(
        data.keys().cloned().collect::<Vec<_>>(),
        ["schema_version", "session", "state", "subagents", "turns"]
    );
    let session = data["session"].as_object().expect("session object");
    assert_eq!(
        session.keys().cloned().collect::<Vec<_>>(),
        [
            "agent_kind",
            "created_at",
            "session_id",
            "state",
            "updated_at"
        ]
    );
    let turn = data["turns"][0].as_object().expect("turn object");
    assert_eq!(
        turn.keys().cloned().collect::<Vec<_>>(),
        [
            "checkpoint_id",
            "completeness",
            "coverage_schema_version",
            "coverage_state",
            "current_revision",
            "logical_turn_key",
            "ordinal",
            "revisions",
            "source_channel",
        ]
    );
    let revision = turn["revisions"][0].as_object().expect("revision object");
    assert_eq!(
        revision.keys().cloned().collect::<Vec<_>>(),
        [
            "checkpoint_id",
            "completeness",
            "created_at",
            "revision",
            "source_channel",
        ]
    );
    let subagents = data["subagents"].as_object().expect("subagents object");
    assert_eq!(
        subagents.keys().cloned().collect::<Vec<_>>(),
        ["available", "nodes", "unavailable_reason"]
    );
    let node = subagents["nodes"][0]
        .as_object()
        .expect("subagent node object");
    assert_eq!(
        node.keys().cloned().collect::<Vec<_>>(),
        [
            "boundary_checkpoint_id",
            "checkpoint_id",
            "created_at",
            "link_state",
        ]
    );
}
