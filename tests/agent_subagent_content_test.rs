//! M5 / DR-06 end-to-end subagent content capture.

#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    str::FromStr,
};

use git_internal::hash::ObjectHash;
use libra::{internal::ai::observed_agents::claude_project_slug, utils::object::read_git_object};
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};

struct Fixture {
    _directory: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path().join("repo");
        let home = directory.path().join("home");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::create_dir_all(&home).expect("home");
        // The hook binds and uses the canonical working directory when it
        // derives Claude's project slug. Keep both fixture identities
        // canonical too: macOS may spell its temp root as `/var` while the
        // verified hook scope is `/private/var`.
        let fixture = Self {
            _directory: directory,
            repo: repo.canonicalize().expect("canonical repo"),
            home: home.canonicalize().expect("canonical home"),
        };
        let output = fixture.run(&["init"], None);
        assert!(output.status.success(), "init: {}", describe(&output));
        fixture
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_with_env(args, stdin, &[])
    }

    fn run_with_env(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn libra");
        if let Some(stdin) = stdin {
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(stdin.as_bytes())
                .expect("write stdin");
        }
        child.wait_with_output().expect("wait libra")
    }

    fn write_transcripts(&self, session_id: &str) -> PathBuf {
        let project = self
            .home
            .join(".claude/projects")
            .join(claude_project_slug(&self.repo));
        let subagents = project.join(session_id).join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents directory");
        let parent = project.join(format!("{session_id}.jsonl"));
        std::fs::write(
            &parent,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "user", "uuid": "parent-user",
                    "message": {"role": "user", "content": "parent"}
                }),
                json!({
                    "type": "assistant", "uuid": "parent-assistant",
                    "message": {"role": "assistant", "content": "done"}
                })
            ),
        )
        .expect("parent transcript");
        std::fs::write(
            subagents.join("child.jsonl"),
            format!(
                "not-json\n{}\n{}\n",
                json!({
                    "type": "user", "uuid": "child-user",
                    "message": {"role": "user", "content": "child"}
                }),
                json!({
                    "type": "assistant", "uuid": "child-assistant",
                    "message": {"role": "assistant", "content": "done"}
                })
            ),
        )
        .expect("child transcript");
        parent
    }

    fn stop(&self, session_id: &str, transcript: &Path) -> Output {
        self.stop_with_env(session_id, transcript, &[])
    }

    fn stop_with_env(&self, session_id: &str, transcript: &Path, env: &[(&str, &str)]) -> Output {
        let envelope = json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "cwd": self.repo,
            "transcript_path": transcript,
        })
        .to_string();
        self.run_with_env(
            &["agent", "hooks", "claude-code", "stop"],
            Some(&envelope),
            env,
        )
    }

    fn checkpoints(&self) -> Vec<Value> {
        let output = self.run(&["agent", "checkpoint", "list", "--json"], None);
        assert!(output.status.success(), "list: {}", describe(&output));
        let value: Value = serde_json::from_slice(&output.stdout).expect("list json");
        value["data"]["checkpoints"]
            .as_array()
            .expect("checkpoint rows")
            .clone()
    }

    /// Reject only the parent checkpoint insert after the independent child
    /// content transaction has had a chance to commit. This uses SQLite's
    /// per-fixture database rather than a process environment fault knob, so
    /// a provider-controlled hook environment cannot influence production
    /// capture behavior.
    async fn reject_committed_checkpoint_writes(&self) {
        let database_path = self.repo.join(".libra/libra.db");
        let conn = Database::connect(format!("sqlite://{}", database_path.display()))
            .await
            .expect("connect repository database");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER reject_committed_checkpoint_writes
             BEFORE INSERT ON agent_checkpoint
             WHEN NEW.scope = 'committed'
             BEGIN
                 SELECT RAISE(ABORT, 'test rejects committed parent checkpoint');
             END"
            .to_string(),
        ))
        .await
        .expect("install parent checkpoint rejection trigger");
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

#[tokio::test]
async fn claude_hook_captures_partial_subagent_content_once_as_unresolved() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000006";
    let transcript = fixture.write_transcripts(session_id);
    let first = fixture.stop(session_id, &transcript);
    assert!(first.status.success(), "first stop: {}", describe(&first));
    let second = fixture.stop(session_id, &transcript);
    assert!(
        second.status.success(),
        "idempotent stop: {}",
        describe(&second)
    );

    let checkpoints = fixture.checkpoints();
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "committed")
            .count(),
        1
    );
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "subagent")
            .count(),
        1,
        "repeat discovery must retain one visible content leaf: {checkpoints:?}"
    );
    let doctor = fixture.run(&["agent", "doctor", "--json"], None);
    assert!(doctor.status.success(), "doctor: {}", describe(&doctor));
    assert!(
        String::from_utf8_lossy(&doctor.stdout).contains("unresolved_subagent_link"),
        "doctor must surface the unresolved current content link: {}",
        describe(&doctor)
    );

    let database_path = fixture.repo.join(".libra/libra.db");
    let conn = Database::connect(format!("sqlite://{}", database_path.display()))
        .await
        .expect("connect repository database");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT c.current_revision, c.current_checkpoint_id, c.source_key,
                    c.content_schema_version,
                    cp.metadata_blob_oid, r.partial, l.link_state,
                    l.boundary_checkpoint_id
             FROM agent_subagent_content_claim c
             JOIN agent_subagent_content_revision r
               ON r.checkpoint_id = c.current_checkpoint_id
             JOIN agent_subagent_link l
               ON l.content_checkpoint_id = c.current_checkpoint_id
             JOIN agent_checkpoint cp
               ON cp.checkpoint_id = c.current_checkpoint_id"
                .to_string(),
        ))
        .await
        .expect("query attribution")
        .expect("attribution row");
    assert_eq!(
        row.try_get_by::<i64, _>("current_revision")
            .expect("revision"),
        1
    );
    assert_eq!(row.try_get_by::<i64, _>("partial").expect("partial"), 1);
    assert_eq!(
        row.try_get_by::<String, _>("link_state")
            .expect("link state"),
        "unresolved"
    );
    assert_eq!(
        row.try_get_by::<Option<String>, _>("boundary_checkpoint_id")
            .expect("boundary"),
        None
    );
    let source_key = row
        .try_get_by::<String, _>("source_key")
        .expect("source key");
    assert!(!Path::new(&source_key).is_absolute());
    assert!(source_key.starts_with("source/subagent-hmac-v2/"));
    assert_eq!(source_key.len(), "source/subagent-hmac-v2/".len() + 64);
    assert_eq!(
        row.try_get_by::<i64, _>("content_schema_version")
            .expect("subagent content schema version"),
        2,
    );
    assert!(!source_key.contains("child.jsonl"));
    assert!(!source_key.contains(session_id));
    let metadata_oid = row
        .try_get_by::<String, _>("metadata_blob_oid")
        .expect("content metadata oid");
    let metadata_hash = ObjectHash::from_str(&metadata_oid).expect("metadata object hash");
    let metadata = read_git_object(&fixture.repo.join(".libra"), &metadata_hash)
        .expect("read content metadata object");
    let metadata_text = String::from_utf8(metadata).expect("metadata JSON is UTF-8");
    assert!(!metadata_text.contains("child.jsonl"));
    assert!(!metadata_text.contains(fixture.repo.to_string_lossy().as_ref()));

    let content_checkpoint_id = row
        .try_get_by::<String, _>("current_checkpoint_id")
        .expect("content checkpoint id");
    // The manual-required finding must name the content checkpoint so the
    // user can act on it with `agent checkpoint show <id>`.
    let doctor_json: Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON output");
    let unresolved = doctor_json["data"]["checkpoint_store"]["findings"]
        .as_array()
        .expect("doctor findings")
        .iter()
        .find(|finding| finding["inconsistency_type"] == "unresolved_subagent_link")
        .expect("unresolved subagent link finding");
    assert_eq!(
        unresolved["checkpoint_id"], content_checkpoint_id,
        "unresolved link finding must report the content checkpoint id: {unresolved}"
    );
    assert_eq!(unresolved["manual_required"], true);
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        [content_checkpoint_id.clone().into()],
    ))
    .await
    .expect("simulate content catalog loss");
    drop(conn);

    let repair = fixture.run(&["agent", "doctor", "--repair", "--json"], None);
    assert!(
        repair.status.success(),
        "doctor repair: {}",
        describe(&repair)
    );
    let repair_json: Value =
        serde_json::from_slice(&repair.stdout).expect("doctor repair JSON output");
    let content_finding = repair_json["data"]["checkpoint_store"]["findings"]
        .as_array()
        .expect("doctor findings")
        .iter()
        .find(|finding| {
            finding["checkpoint_id"] == content_checkpoint_id
                && finding["inconsistency_type"] == "missing_catalog_row"
        })
        .expect("missing content checkpoint finding");
    assert_eq!(content_finding["inconsistency_type"], "missing_catalog_row");
    assert_eq!(content_finding["manual_required"], true);
    assert_eq!(content_finding["repaired"], false);
    assert!(
        repair_json["data"]["checkpoint_store"]["findings"]
            .as_array()
            .expect("doctor findings")
            .iter()
            .any(|finding| {
                finding["checkpoint_id"] == content_checkpoint_id
                    && finding["inconsistency_type"] == "inconsistent_subagent_content"
            }),
        "doctor must diagnose the broken current claim/revision/link companion relation"
    );

    let replay = fixture.stop(session_id, &transcript);
    assert!(
        !replay.status.success(),
        "dangling current content must not be reported as an unchanged success: {}",
        describe(&replay)
    );
    let stderr = String::from_utf8_lossy(&replay.stderr);
    assert!(
        stderr.contains(
            "capture could not be completed; retry the hook or inspect the local repository"
        ),
        "replay error must retain the fixed sanitized diagnostic: {}",
        describe(&replay)
    );
    assert!(
        !stderr.contains("current leaf is incomplete")
            && !stderr.contains(session_id)
            && !stderr.contains(fixture.repo.to_string_lossy().as_ref()),
        "hook stderr must not expose the internal recovery chain or provider-controlled identity: {}",
        describe(&replay)
    );
}

#[tokio::test]
async fn child_content_is_durable_before_parent_checkpoint_advertises_attribution() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000007";
    let transcript = fixture.write_transcripts(session_id);
    fixture.reject_committed_checkpoint_writes().await;
    let failed = fixture.stop(session_id, &transcript);
    assert!(
        !failed.status.success(),
        "the parent checkpoint rejection must interrupt the stop hook: {}",
        describe(&failed)
    );

    let checkpoints = fixture.checkpoints();
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "subagent")
            .count(),
        1,
        "child evidence must already be durable"
    );
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "committed")
            .count(),
        0,
        "parent must not advertise child-derived attribution first"
    );
}

/// The parent checkpoint may derive aggregate child usage, but it must only
/// do so from the capture snapshot's redacted child bytes. A secret in a
/// child transcript therefore cannot reach the parent's durable metadata.
#[tokio::test]
async fn parent_metadata_uses_redacted_child_snapshot_for_attribution() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-00000000000b";
    let transcript = fixture.write_transcripts(session_id);
    let secret = format!("ghp_{}", "c".repeat(36));
    let child_path = fixture
        .home
        .join(".claude/projects")
        .join(claude_project_slug(&fixture.repo))
        .join(session_id)
        .join("subagents/child.jsonl");
    std::fs::write(
        &child_path,
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "uuid": "child-secret",
                "message": {
                    "role": "assistant",
                    "content": format!("child handled {secret}"),
                    "usage": {"input_tokens": 13, "output_tokens": 5}
                }
            })
        ),
    )
    .expect("write secret-bearing child transcript");

    let stop = fixture.stop(session_id, &transcript);
    assert!(stop.status.success(), "stop: {}", describe(&stop));
    let checkpoint_id = fixture
        .checkpoints()
        .into_iter()
        .find(|row| row["scope"] == "committed")
        .and_then(|row| row["checkpoint_id"].as_str().map(str::to_owned))
        .expect("committed parent checkpoint");
    let show = fixture.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
    );
    assert!(
        show.status.success(),
        "checkpoint show: {}",
        describe(&show)
    );
    let show_json: Value = serde_json::from_slice(&show.stdout).expect("checkpoint show JSON");
    assert_eq!(
        show_json["data"]["checkpoint"]["checkpoint_id"],
        json!(checkpoint_id),
        "default show retains only the requested checkpoint identity"
    );
    assert!(
        show_json["data"].get("metadata").is_none(),
        "default show must withhold the metadata document: {show_json}"
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_text.contains(&secret),
        "child secret leaked through default checkpoint show: {show_text}"
    );

    // The parent aggregate is a durable invariant, not a public `show`
    // schema. Read the object directly so this test does not restore the
    // withheld metadata body to the CLI contract.
    let database_path = fixture.repo.join(".libra/libra.db");
    let conn = Database::connect(format!("sqlite://{}?mode=ro", database_path.display()))
        .await
        .expect("connect repository database for metadata assertion");
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [checkpoint_id.clone().into()],
        ))
        .await
        .expect("query parent metadata object")
        .expect("parent checkpoint row");
    let metadata_oid: String = row
        .try_get_by("metadata_blob_oid")
        .expect("parent metadata object id");
    drop(conn);
    let metadata_hash = ObjectHash::from_str(&metadata_oid).expect("metadata object hash");
    let metadata: Value = serde_json::from_slice(
        &read_git_object(&fixture.repo.join(".libra"), &metadata_hash)
            .expect("read parent metadata object"),
    )
    .expect("parent metadata JSON");
    let metadata_text = serde_json::to_string(&metadata).expect("serialize parent metadata");
    assert!(
        !metadata_text.contains(&secret),
        "child secret leaked into durable parent metadata: {metadata_text}"
    );
    let extraction = &metadata["extraction"];
    assert_eq!(extraction["subagent_source_count"], json!(1));
    assert_eq!(
        extraction["subagent_token_usage"]["input_tokens"],
        json!(13),
        "redacted child snapshot must retain aggregate attribution: {extraction}"
    );
    assert_eq!(
        extraction["subagent_snapshot"]["complete_source_count"],
        json!(1),
        "parent metadata must record that the child crossed the safe snapshot boundary"
    );
    assert_eq!(
        extraction["subagent_snapshot"]["partial_source_count"],
        json!(0)
    );
}

#[tokio::test]
async fn unchanged_replay_rejects_an_uncataloged_traces_ancestor() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-00000000000a";
    let transcript = fixture.write_transcripts(session_id);
    let first = fixture.stop(session_id, &transcript);
    assert!(first.status.success(), "first stop: {}", describe(&first));

    let database_path = fixture.repo.join(".libra/libra.db");
    let conn = Database::connect(format!("sqlite://{}", database_path.display()))
        .await
        .expect("connect repository database");
    let deleted = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DELETE FROM agent_checkpoint WHERE scope = 'committed'".to_string(),
        ))
        .await
        .expect("remove catalog row for traces head");
    assert_eq!(deleted.rows_affected(), 1);
    drop(conn);

    let replay = fixture.stop(session_id, &transcript);
    assert!(
        !replay.status.success(),
        "uncataloged traces history must fail replay closed: {}",
        describe(&replay)
    );
    let stderr = String::from_utf8_lossy(&replay.stderr);
    assert!(
        stderr.contains(
            "capture could not be completed; retry the hook or inspect the local repository"
        ),
        "uncataloged history error must retain the fixed sanitized diagnostic: {}",
        describe(&replay)
    );
    assert!(
        !stderr.contains("traces reachability are incomplete")
            && !stderr.contains(session_id)
            && !stderr.contains(fixture.repo.to_string_lossy().as_ref()),
        "hook stderr must not expose the internal recovery chain or provider-controlled identity: {}",
        describe(&replay)
    );
}

#[test]
fn untrusted_discovery_test_environment_cannot_change_live_capture() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000008";
    let transcript = fixture.write_transcripts(session_id);
    let output = fixture.stop_with_env(
        session_id,
        &transcript,
        &[
            // This was a historical debug-binary test knob. The ordinary
            // hook executable must not let an untrusted environment force an
            // already-expired discovery deadline.
            ("LIBRA_TEST_SUBAGENT_DISCOVERY_DEADLINE_MS", "0"),
        ],
    );
    assert!(
        output.status.success(),
        "partial parent stop: {}",
        describe(&output)
    );
    let checkpoints = fixture.checkpoints();
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "subagent")
            .count(),
        1,
        "untrusted deadline environment must not suppress normal child capture"
    );
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "committed")
            .count(),
        1,
        "untrusted deadline environment must not prevent the parent checkpoint"
    );
}

#[test]
fn unchanged_durability_probe_replays_without_checkpoint_duplication() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000009";
    let transcript = fixture.write_transcripts(session_id);
    let first = fixture.stop(session_id, &transcript);
    assert!(first.status.success(), "first stop: {}", describe(&first));
    let checkpoints_before = fixture.checkpoints();
    let repeated = fixture.stop(session_id, &transcript);
    assert!(
        repeated.status.success(),
        "a normal durability replay must remain successful: {}",
        describe(&repeated)
    );
    assert_eq!(
        fixture.checkpoints(),
        checkpoints_before,
        "unchanged durability replay must not duplicate a checkpoint"
    );
}
