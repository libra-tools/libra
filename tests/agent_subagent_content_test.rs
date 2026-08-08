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
        let fixture = Self {
            _directory: directory,
            repo,
            home,
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
    assert!(source_key.starts_with("source/sha256/"));
    assert_eq!(source_key.len(), "source/sha256/".len() + 64);
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
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("libra agent doctor"),
        "replay error must be actionable: {}",
        describe(&replay)
    );
}

#[tokio::test]
async fn child_content_is_durable_before_parent_checkpoint_advertises_attribution() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000007";
    let transcript = fixture.write_transcripts(session_id);
    let failed = fixture.stop_with_env(
        session_id,
        &transcript,
        &[(
            "LIBRA_TEST_FAIL_AFTER_SUBAGENT_CONTENT_BEFORE_PARENT_CHECKPOINT",
            "1",
        )],
    );
    assert!(
        !failed.status.success(),
        "injected stop: {}",
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
        stderr.contains("traces reachability are incomplete")
            && stderr.contains("libra agent doctor"),
        "uncataloged history error must identify the repair path: {}",
        describe(&replay)
    );
}

#[test]
fn blocking_child_discovery_is_killed_at_live_deadline() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000008";
    let transcript = fixture.write_transcripts(session_id);
    let started = std::time::Instant::now();
    let output = fixture.stop_with_env(
        session_id,
        &transcript,
        &[
            ("LIBRA_TEST_SUBAGENT_DISCOVERY_HELPER_DELAY_MS", "5000"),
            ("LIBRA_TEST_SUBAGENT_DISCOVERY_DEADLINE_MS", "80"),
        ],
    );
    assert!(
        output.status.success(),
        "partial parent stop: {}",
        describe(&output)
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "blocking helper exceeded the parent deadline: {:?}",
        started.elapsed()
    );
    let checkpoints = fixture.checkpoints();
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "subagent")
            .count(),
        0
    );
    assert_eq!(
        checkpoints
            .iter()
            .filter(|row| row["scope"] == "committed")
            .count(),
        1,
        "live discovery timeout preserves a partial parent checkpoint"
    );
}

#[test]
fn blocking_unchanged_durability_probe_is_killed_at_live_deadline() {
    let fixture = Fixture::new();
    let session_id = "abcdef00-0000-0000-0000-000000000009";
    let transcript = fixture.write_transcripts(session_id);
    let first = fixture.stop(session_id, &transcript);
    assert!(first.status.success(), "first stop: {}", describe(&first));

    let ready = fixture.repo.join("verify-helper.ready");
    let ready_text = ready.to_string_lossy().into_owned();
    let started = std::time::Instant::now();
    let repeated = fixture.stop_with_env(
        session_id,
        &transcript,
        &[
            ("LIBRA_TEST_CHECKPOINT_VERIFY_READY_FILE", &ready_text),
            ("LIBRA_TEST_SUBAGENT_DISCOVERY_DEADLINE_MS", "120"),
        ],
    );
    assert!(
        !repeated.status.success(),
        "a killed durability probe must fail the replay closed: {}",
        describe(&repeated)
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "blocking durability helper exceeded the parent deadline: {:?}",
        started.elapsed()
    );
    assert!(
        ready.exists(),
        "the killable verification helper was not reached"
    );
    assert!(
        String::from_utf8_lossy(&repeated.stderr).contains("libra agent doctor"),
        "killed durability verification must fail closed with a recovery path: {}",
        describe(&repeated)
    );
}
