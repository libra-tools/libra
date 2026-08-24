//! plan-20260713 M4 historical transcript import contract.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use git_internal::hash::ObjectHash;
use libra::{
    internal::{
        ai::{
            agent_import::restore_tombstone,
            history::{HistoryManager, TracesInflightMarker, write_traces_inflight_marker},
            observed_agents::AgentKind,
        },
        branch::TRACES_BRANCH,
    },
    utils::{client_storage::ClientStorage, object::read_git_object},
};
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use tempfile::TempDir;

struct ImportRepo {
    _tmp: TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl ImportRepo {
    fn init() -> Self {
        let tmp = TempDir::new().expect("create tempdir");
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::create_dir_all(&home).expect("create home dir");
        // The binary runs with `current_dir(repo)` and therefore sees the
        // canonical path, which is what it hashes into the Claude project
        // slug and compares against for containment. `tempfile` hands back the
        // caller's spelling, and stock macOS reaches TMPDIR through
        // `/var -> private/var`, so an uncanonicalized fixture path makes
        // discovery look in a directory the binary never writes to.
        let repo = repo.canonicalize().expect("canonical repo dir");
        let home = home.canonicalize().expect("canonical home dir");
        let fixture = Self {
            _tmp: tmp,
            repo,
            home,
        };
        let output = fixture.run(&["init"]);
        assert!(output.status.success(), "init: {}", describe(&output));
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

    fn run_with_env(&self, args: &[&str], key: &str, value: &str) -> Output {
        self.command()
            .env(key, value)
            .args(args)
            .output()
            .expect("run libra with test env")
    }

    fn session_end_hook(&self, session_id: &str, transcript_path: &Path) -> Output {
        let envelope = json!({
            "hook_event_name": "SessionEnd",
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
            "transcript_path": transcript_path.to_string_lossy(),
        })
        .to_string();
        let mut child = self
            .command()
            .args(["agent", "hooks", "claude-code", "session-end"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn live hook writer");
        child
            .stdin
            .as_mut()
            .expect("hook stdin is piped")
            .write_all(envelope.as_bytes())
            .expect("write hook envelope");
        child.wait_with_output().expect("wait for hook writer")
    }

    fn transcript_path(&self, session_id: &str) -> PathBuf {
        self.home
            .join(".claude")
            .join("projects")
            .join("fixture")
            .join(format!("{session_id}.jsonl"))
    }

    fn discoverable_transcript_path(&self, session_id: &str) -> PathBuf {
        let slug = self
            .repo
            .to_string_lossy()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect::<String>();
        self.home
            .join(".claude")
            .join("projects")
            .join(slug)
            .join(format!("{session_id}.jsonl"))
    }

    fn write_transcript(&self, session_id: &str, cwd: &Path, complete: bool) -> PathBuf {
        let path = self.transcript_path(session_id);
        std::fs::create_dir_all(path.parent().expect("transcript parent"))
            .expect("create transcript dir");
        let mut lines = vec![json!({
            "type": "user",
            "uuid": "turn-1",
            "sessionId": session_id,
            "cwd": cwd,
            "timestamp": "2026-07-15T01:00:00Z",
            "unknown_private": "AKIAAAAAAAAAAAAAAAAA",
            "message": {"role": "user", "content": "inspect the repo"}
        })];
        if complete {
            lines.push(json!({
                "type": "assistant",
                "uuid": "assistant-1",
                "sessionId": session_id,
                "cwd": cwd,
                "timestamp": "2026-07-15T01:00:01Z",
                "provider_private": "drop-this-field",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
            }));
            lines.push(json!({
                "type": "session_end",
                "sessionId": session_id,
                "cwd": cwd,
                "timestamp": "2026-07-15T01:00:02Z"
            }));
        }
        let mut body = lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if !complete {
            body.push_str(
                "\n{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"te",
            );
        }
        std::fs::write(&path, format!("{body}\n")).expect("write transcript");
        path
    }

    fn write_discoverable_transcript(&self, session_id: &str, cwd: &Path) -> PathBuf {
        let source = self.write_transcript(session_id, cwd, true);
        let destination = self.discoverable_transcript_path(session_id);
        std::fs::create_dir_all(destination.parent().expect("discovery parent"))
            .expect("create discovery dir");
        std::fs::rename(source, &destination).expect("move transcript into discovery dir");
        destination
    }

    async fn scalar(&self, sql: &str) -> i64 {
        let url = format!(
            "sqlite://{}?mode=ro",
            self.repo.join(".libra/libra.db").display()
        );
        let conn = Database::connect(url).await.expect("open repo db");
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("query db")
        .expect("one row")
        .try_get_by("n")
        .expect("integer result")
    }

    async fn text_rows(&self, sql: &str, column: &str) -> Vec<String> {
        let url = format!(
            "sqlite://{}?mode=ro",
            self.repo.join(".libra/libra.db").display()
        );
        let conn = Database::connect(url).await.expect("open repo db");
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("query db")
        .into_iter()
        .map(|row| row.try_get_by(column).expect("text result"))
        .collect()
    }

    async fn repo_id(&self) -> String {
        self.text_rows(
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1",
            "value",
        )
        .await
        .into_iter()
        .next()
        .expect("initialized repository identity")
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

fn loose_object_file_count(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .count()
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Create a directory whose name is deliberately not valid UTF-8.
///
/// Returns `None` when the filesystem refuses the name outright (`EILSEQ`),
/// which is what stock macOS APFS/HFS+ does: those volumes only accept valid
/// UTF-8 filenames, so the byte sequence the caller wants to exercise cannot
/// exist there at all. Callers skip in that case rather than fail — the
/// behaviour under test is the binary's handling of such a path, not the
/// platform's willingness to create one.
#[cfg(unix)]
fn create_non_utf8_dir(path: &std::path::Path) -> Option<()> {
    match std::fs::create_dir_all(path) {
        Ok(()) => Some(()),
        Err(error) if error.raw_os_error() == Some(92) => None,
        Err(error) => panic!("create non-UTF-8 dir {}: {error}", path.display()),
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_repository_is_rejected_with_stable_io_code_before_import() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let tmp = TempDir::new().expect("create non-UTF-8 import tempdir");
    let repo = tmp.path().join(OsString::from_vec(b"repo-\xff".to_vec()));
    if create_non_utf8_dir(&repo).is_none() {
        eprintln!("skipped (filesystem rejects non-UTF-8 directory names)");
        return;
    }
    let init = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&repo)
        .arg("init")
        .output()
        .expect("attempt non-UTF-8 repo initialization");
    assert!(
        !init.status.success(),
        "non-UTF-8 repo unexpectedly initialized"
    );
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("LBR-IO-001"),
        "non-UTF-8 repo rejection lost its stable code: {}",
        describe(&init)
    );
}

#[cfg(unix)]
#[test]
fn agent_import_accepts_non_utf8_provider_root_via_lossless_helper_wire() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let tmp = TempDir::new().expect("create non-UTF-8 provider tempdir");
    let repo = tmp.path().join("repo");
    let home = tmp.path().join(OsString::from_vec(b"home-\xfe".to_vec()));
    std::fs::create_dir_all(&repo).expect("create repo");
    if create_non_utf8_dir(&home).is_none() {
        eprintln!("skipped (filesystem rejects non-UTF-8 directory names)");
        return;
    }
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("LIBRA_TEST_HOME", &home)
            .env_remove("CODEX_HOME");
        command
    };
    let init = command().arg("init").output().expect("initialize repo");
    assert!(init.status.success(), "init: {}", describe(&init));
    let project_slug = repo
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let transcript = home
        .join(".claude/projects")
        .join(project_slug)
        .join("abc123.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    let body = [
        json!({
            "type": "user", "uuid": "turn-1", "sessionId": "abc123",
            "cwd": repo, "message": {"role": "user", "content": "inspect"}
        }),
        json!({
            "type": "assistant", "uuid": "answer-1", "sessionId": "abc123",
            "cwd": repo,
            "message": {"role": "assistant", "content": [{"type":"text", "text":"done"}]}
        }),
        json!({"type": "session_end", "sessionId": "abc123", "cwd": repo}),
    ]
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(&transcript, format!("{body}\n")).expect("write transcript");
    let output = command()
        .args([
            "agent",
            "import",
            "--session",
            "abc123",
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run non-UTF-8 import");
    assert!(
        output.status.success(),
        "lossless helper wire rejected non-UTF-8 provider root: {}",
        describe(&output)
    );
}

#[test]
fn agent_import_explicit_session_accepts_safe_legacy_claude_identifier() {
    let fixture = ImportRepo::init();
    let session_id = "Legacy.session_01";
    let transcript = fixture.discoverable_transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    let body = [
        json!({
            "type": "user", "uuid": "turn-1", "sessionId": session_id,
            "cwd": fixture.repo, "message": {"role": "user", "content": "inspect"}
        }),
        json!({
            "type": "assistant", "uuid": "answer-1", "sessionId": session_id,
            "cwd": fixture.repo,
            "message": {"role": "assistant", "content": [{"type":"text", "text":"done"}]}
        }),
        json!({"type": "session_end", "sessionId": session_id, "cwd": fixture.repo}),
    ]
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(&transcript, format!("{body}\n")).expect("write legacy transcript");
    let output = fixture.run(&[
        "agent",
        "import",
        "--session",
        session_id,
        "--yes",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "safe legacy explicit session id was accepted by CLI but rejected by resolver: {}",
        describe(&output)
    );
}

fn tree_entry_oid(tree: &[u8], wanted: &str) -> String {
    let mut cursor = 0usize;
    while cursor < tree.len() {
        let mode_end = tree[cursor..]
            .iter()
            .position(|byte| *byte == b' ')
            .map(|offset| cursor + offset)
            .expect("tree entry mode delimiter");
        let name_start = mode_end + 1;
        let name_end = tree[name_start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| name_start + offset)
            .expect("tree entry name delimiter");
        let oid_start = name_end + 1;
        let oid_end = oid_start + 20;
        assert!(oid_end <= tree.len(), "tree entry object id is truncated");
        if &tree[name_start..name_end] == wanted.as_bytes() {
            return hex::encode(&tree[oid_start..oid_end]);
        }
        cursor = oid_end;
    }
    panic!("tree entry '{wanted}' not found");
}

fn read_object_payload(storage_root: &Path, oid: &str) -> Vec<u8> {
    let oid = ObjectHash::from_str(oid).expect("valid test object id");
    read_git_object(storage_root, &oid).expect("read test object")
}

fn read_checkpoint_blob(
    storage_root: &Path,
    root_tree_oid: &str,
    checkpoint_id: &str,
    relative_path: &[&str],
) -> Vec<u8> {
    let mut oid = root_tree_oid.to_string();
    for component in ["checkpoint", &checkpoint_id[..2], &checkpoint_id[2..]]
        .into_iter()
        .chain(relative_path.iter().copied())
    {
        let tree = read_object_payload(storage_root, &oid);
        oid = tree_entry_oid(&tree, component);
    }
    read_object_payload(storage_root, &oid)
}

#[tokio::test]
async fn agent_import_derives_working_dir_and_is_idempotent() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("abc123", &fixture.repo, true);
    let transcript = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];

    let first = fixture.run(&args);
    assert!(first.status.success(), "first import: {}", describe(&first));
    let json: Value = serde_json::from_slice(&first.stdout).expect("import JSON");
    assert_eq!(json["data"]["results"][0]["status"], "imported");
    assert_eq!(json["data"]["results"][0]["checkpoints_written"], 1);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_identity")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
            .await,
        1
    );
    assert_eq!(
        fixture
            .text_rows("SELECT working_dir FROM agent_session", "working_dir")
            .await,
        vec![
            fixture
                .repo
                .canonicalize()
                .expect("canonical repo")
                .to_string_lossy()
                .into_owned(),
        ]
    );

    let second = fixture.run(&args);
    assert!(
        second.status.success(),
        "replay import: {}",
        describe(&second)
    );
    let json: Value = serde_json::from_slice(&second.stdout).expect("replay JSON");
    assert_eq!(json["data"]["results"][0]["status"], "noop");
    assert_eq!(json["data"]["results"][0]["checkpoints_written"], 0);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );

    let db = std::fs::read(fixture.repo.join(".libra/libra.db")).expect("read sqlite file");
    assert!(
        !db.windows("AKIAAAAAAAAAAAAAAAAA".len())
            .any(|window| window == b"AKIAAAAAAAAAAAAAAAAA"),
        "unknown provider fields and raw secrets must not persist"
    );
}

#[tokio::test]
async fn agent_import_skips_live_covered_turn_and_merges_terminal_lifecycle() {
    let fixture = ImportRepo::init();
    let session_id = "live-covered-import";
    let transcript = fixture.write_transcript(session_id, &fixture.repo, true);
    let live = fixture.session_end_hook(session_id, &transcript);
    assert!(live.status.success(), "live hook: {}", describe(&live));
    let transcript_arg = path_arg(&transcript);

    let imported = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        imported.status.success(),
        "covered import: {}",
        describe(&imported)
    );
    let body: Value = serde_json::from_slice(&imported.stdout).expect("covered import JSON");
    assert_eq!(body["data"]["results"][0]["status"], "noop");
    assert_eq!(body["data"]["results"][0]["checkpoints_written"], 0);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1,
        "import must not duplicate a live-covered turn"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        1,
        "covered no-op must not append a duplicate revision"
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT state FROM agent_session WHERE provider_session_id = 'live-covered-import'",
                "state"
            )
            .await,
        vec!["stopped"],
        "terminal no-op import must still merge its lifecycle state"
    );
}

#[tokio::test]
async fn agent_import_commit_before_live_hook_has_one_defined_revision() {
    let fixture = ImportRepo::init();
    let session_id = "import-before-live";
    let transcript = fixture.write_transcript(session_id, &fixture.repo, true);
    let imported = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(imported.status.success(), "import: {}", describe(&imported));

    let live = fixture.session_end_hook(session_id, &transcript);
    assert!(
        live.status.success(),
        "live replay after import: {}",
        describe(&live)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1,
        "live hook must recognize the import-covered turn"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        1,
        "import-first/live-second ordering must have one defined revision"
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT state FROM agent_session WHERE provider_session_id = 'import-before-live'",
                "state"
            )
            .await,
        vec!["stopped"],
        "a covered live replay must not regress terminal session state"
    );
}

#[tokio::test]
async fn agent_import_same_digest_terminal_upgrade_writes_complete_schema_valid_revision() {
    let fixture = ImportRepo::init();
    let session_id = "terminalupgrade";
    let secret = "AKIAIOSFODNN7EXAMPLE";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let semantic_lines = [
        json!({
            "type": "user",
            "uuid": "upgrade-turn",
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:00Z",
            "message": {"role": "user", "content": format!("inspect with {secret}")}
        }),
        json!({
            "type": "assistant",
            "uuid": "upgrade-answer",
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:01Z",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "done"},
                {"type": "tool_use", "id": "tool-secret-key", "name": "inspect",
                 "input": {(secret): "value"}}
            ]}
        }),
    ];
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            semantic_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write growing transcript");
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run(&args);
    assert!(first.status.success(), "{}", describe(&first));
    assert_eq!(
        fixture
            .text_rows(
                "SELECT state FROM agent_session WHERE provider_session_id = 'terminalupgrade'",
                "state"
            )
            .await,
        vec!["active"]
    );

    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open transcript for terminal evidence"),
        "{}",
        json!({
            "type": "session_end",
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:02Z"
        })
    )
    .expect("append terminal evidence");
    let second = fixture.run(&args);
    assert!(second.status.success(), "{}", describe(&second));
    let second_json: Value = serde_json::from_slice(&second.stdout).expect("upgrade JSON");
    assert_eq!(second_json["data"]["results"][0]["status"], "imported");
    assert_eq!(second_json["data"]["results"][0]["checkpoints_written"], 1);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        2
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        2
    );

    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open result db");
    let lifecycle = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT state, last_event_at, stopped_at FROM agent_session
             WHERE provider_session_id = 'terminalupgrade'"
                .to_string(),
        ))
        .await
        .expect("query lifecycle")
        .expect("lifecycle row");
    assert_eq!(
        lifecycle.try_get_by::<String, _>("state").unwrap(),
        "stopped"
    );
    assert_eq!(
        lifecycle.try_get_by::<i64, _>("last_event_at").unwrap(),
        1_784_077_202
    );
    assert_eq!(
        lifecycle.try_get_by::<i64, _>("stopped_at").unwrap(),
        1_784_077_202
    );
    let checkpoint = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT r.checkpoint_id, c.tree_oid
             FROM agent_coverage_revision r
             JOIN agent_checkpoint c ON c.checkpoint_id = r.checkpoint_id
             WHERE r.session_id = 'claude__terminalupgrade'
             ORDER BY r.revision DESC LIMIT 1"
                .to_string(),
        ))
        .await
        .expect("query upgraded checkpoint")
        .expect("upgraded checkpoint");
    let checkpoint_id: String = checkpoint.try_get_by("checkpoint_id").unwrap();
    let tree_oid: String = checkpoint.try_get_by("tree_oid").unwrap();
    db.close().await.expect("close result db");

    let storage = fixture.repo.join(".libra");
    let metadata = read_checkpoint_blob(&storage, &tree_oid, &checkpoint_id, &["metadata.json"]);
    let metadata: Value = serde_json::from_slice(&metadata).expect("metadata schema JSON");
    assert_eq!(metadata["schema_version"], 2);
    assert_eq!(metadata["model"], "unknown");
    assert_eq!(metadata["redaction_report"]["raw_persisted"], false);
    assert!(
        metadata["redaction_report"]["bytes_redacted"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );

    let transcript_blob = read_checkpoint_blob(
        &storage,
        &tree_oid,
        &checkpoint_id,
        &["transcript", "claude_code.jsonl"],
    );
    assert!(
        !transcript_blob
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
    );
    let transcript_lines = transcript_blob
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(transcript_lines.len(), 1);
    for line in transcript_lines {
        serde_json::from_slice::<Value>(line).expect("transcript line is JSON");
    }

    let lifecycle_blob = read_checkpoint_blob(
        &storage,
        &tree_oid,
        &checkpoint_id,
        &["events", "lifecycle.jsonl"],
    );
    for line in lifecycle_blob
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let event: Value = serde_json::from_slice(line).expect("lifecycle line is JSON");
        for key in [
            "schema_version",
            "event_id",
            "kind",
            "agent_kind",
            "session_id",
            "provider_session_id",
            "timestamp",
            "source",
            "partial",
            "provenance",
        ] {
            assert!(event.get(key).is_some(), "missing lifecycle key {key}");
        }
        uuid::Uuid::parse_str(event["event_id"].as_str().expect("event id string"))
            .expect("event id UUID");
        assert_eq!(event["partial"], false);
    }

    let report = read_checkpoint_blob(
        &storage,
        &tree_oid,
        &checkpoint_id,
        &["redaction_report.json"],
    );
    let report: Value = serde_json::from_slice(&report).expect("redaction report JSON");
    assert!(
        report["bytes_redacted"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        report["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty())
    );
    assert!(
        report["matches"].as_array().is_some_and(|matches| matches
            .iter()
            .any(|entry| entry["rule_id"] == "aws-access-key-id")),
        "tool-input object keys must contribute redaction evidence"
    );

    let manifest = read_checkpoint_blob(&storage, &tree_oid, &checkpoint_id, &["manifest.json"]);
    let manifest: Value = serde_json::from_slice(&manifest).expect("manifest JSON");
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(
        manifest["entries"]["transcript"]["media_type"],
        "application/x-ndjson"
    );
}

#[tokio::test]
async fn agent_import_preserves_three_turn_checkpoint_and_lifecycle_chronology() {
    let fixture = ImportRepo::init();
    let session_id = "chronology123";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let mut lines = Vec::new();
    for (index, minute) in [0_u32, 10, 20].into_iter().enumerate() {
        lines.push(json!({
            "type": "user",
            "uuid": format!("chronology-turn-{index}"),
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": format!("2026-07-15T01:{minute:02}:00Z"),
            "message": {"role": "user", "content": format!("question {index}")}
        }));
        lines.push(json!({
            "type": "assistant",
            "uuid": format!("chronology-answer-{index}"),
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": format!("2026-07-15T01:{minute:02}:01Z"),
            "message": {"role": "assistant", "content": format!("answer {index}")}
        }));
    }
    lines.push(json!({
        "type": "session_end",
        "sessionId": session_id,
        "cwd": fixture.repo,
        "timestamp": "2026-07-15T01:20:02Z"
    }));
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write chronology transcript");
    let transcript_arg = path_arg(&transcript);
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{}", describe(&output));
    let payload: Value = serde_json::from_slice(&output.stdout).expect("import JSON");
    assert_eq!(payload["data"]["results"][0]["checkpoints_written"], 3);

    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open chronology db");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, tree_oid, created_at FROM agent_checkpoint
             WHERE session_id = 'claude__chronology123'
             ORDER BY created_at ASC, checkpoint_id ASC"
                .to_string(),
        ))
        .await
        .expect("query checkpoint chronology");
    let expected_times = [1_784_077_201_i64, 1_784_077_801, 1_784_078_401];
    assert_eq!(rows.len(), expected_times.len());
    let mut ascending_ids = Vec::new();
    for (row, expected_time) in rows.iter().zip(expected_times) {
        let checkpoint_id: String = row.try_get_by("checkpoint_id").unwrap();
        let tree_oid: String = row.try_get_by("tree_oid").unwrap();
        assert_eq!(
            row.try_get_by::<i64, _>("created_at").unwrap(),
            expected_time
        );
        ascending_ids.push(checkpoint_id.clone());
        let metadata = read_checkpoint_blob(
            &fixture.repo.join(".libra"),
            &tree_oid,
            &checkpoint_id,
            &["metadata.json"],
        );
        let metadata: Value = serde_json::from_slice(&metadata).expect("metadata JSON");
        assert_eq!(metadata["created_at"], expected_time);
        assert_eq!(metadata["turn_ended_at"], expected_time);
        let lifecycle = read_checkpoint_blob(
            &fixture.repo.join(".libra"),
            &tree_oid,
            &checkpoint_id,
            &["events", "lifecycle.jsonl"],
        );
        let event: Value = serde_json::from_slice(
            lifecycle
                .split(|byte| *byte == b'\n')
                .find(|line| !line.is_empty())
                .expect("lifecycle line"),
        )
        .expect("lifecycle JSON");
        let actual = chrono::DateTime::parse_from_rfc3339(
            event["timestamp"].as_str().expect("timestamp string"),
        )
        .expect("RFC3339 lifecycle timestamp")
        .timestamp();
        assert_eq!(actual, expected_time);
    }
    db.close().await.expect("close chronology db");

    let list = fixture.run(&["agent", "checkpoint", "list", "--json"]);
    assert!(list.status.success(), "{}", describe(&list));
    let list: Value = serde_json::from_slice(&list.stdout).expect("checkpoint list JSON");
    let listed_ids = list["data"]["checkpoints"]
        .as_array()
        .expect("checkpoint list")
        .iter()
        .map(|row| row["checkpoint_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ascending_ids.reverse();
    assert_eq!(listed_ids, ascending_ids);
}

#[tokio::test]
async fn agent_import_normalizes_same_second_turns_in_public_checkpoint_order() {
    let fixture = ImportRepo::init();
    let session_id = "same-second-chronology";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let mut lines = Vec::new();
    for index in 0..3 {
        lines.push(json!({
            "type": "user",
            "uuid": format!("same-second-turn-{index}"),
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:00.100Z",
            "message": {"role": "user", "content": format!("question {index}")}
        }));
        lines.push(json!({
            "type": "assistant",
            "uuid": format!("same-second-answer-{index}"),
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:00.900Z",
            "message": {"role": "assistant", "content": format!("answer {index}")}
        }));
    }
    lines.push(json!({
        "type": "session_end",
        "sessionId": session_id,
        "cwd": fixture.repo,
        "timestamp": "2026-07-15T01:00:00.999Z"
    }));
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write same-second transcript");
    let imported = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(imported.status.success(), "{}", describe(&imported));

    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open same-second chronology db");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, created_at FROM agent_checkpoint
             WHERE session_id = 'claude__same-second-chronology'
             ORDER BY created_at ASC"
                .to_string(),
        ))
        .await
        .expect("query normalized chronology");
    assert_eq!(rows.len(), 3);
    let expected_times = [1_784_077_200_i64, 1_784_077_201, 1_784_077_202];
    let mut expected_public_ids = Vec::new();
    for (row, expected_time) in rows.iter().zip(expected_times) {
        assert_eq!(
            row.try_get_by::<i64, _>("created_at").unwrap(),
            expected_time
        );
        expected_public_ids.push(row.try_get_by::<String, _>("checkpoint_id").unwrap());
    }
    let lifecycle = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT last_event_at, stopped_at FROM agent_session
             WHERE session_id = 'claude__same-second-chronology'"
                .to_string(),
        ))
        .await
        .expect("query normalized session lifecycle")
        .expect("normalized session row");
    assert_eq!(
        lifecycle.try_get_by::<i64, _>("last_event_at").unwrap(),
        expected_times[2]
    );
    assert_eq!(
        lifecycle.try_get_by::<i64, _>("stopped_at").unwrap(),
        expected_times[2]
    );
    db.close().await.expect("close same-second chronology db");

    let listed = fixture.run(&["agent", "checkpoint", "list", "--json"]);
    assert!(listed.status.success(), "{}", describe(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("checkpoint list JSON");
    let listed_ids = listed["data"]["checkpoints"]
        .as_array()
        .expect("checkpoint list")
        .iter()
        .map(|row| row["checkpoint_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    expected_public_ids.reverse();
    assert_eq!(listed_ids, expected_public_ids);
}

#[tokio::test]
async fn agent_import_duplicate_provider_turn_ids_do_not_silently_drop_turns() {
    let fixture = ImportRepo::init();
    let session_id = "duplicate-turn-ids";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let lines = [
        json!({"type":"user","uuid":"duplicate","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:00Z","message":{"role":"user","content":"first"}}),
        json!({"type":"assistant","uuid":"answer-1","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:01Z","message":{"role":"assistant","content":"one"}}),
        json!({"type":"user","uuid":"duplicate","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:02Z","message":{"role":"user","content":"second"}}),
        json!({"type":"assistant","uuid":"answer-2","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:03Z","message":{"role":"assistant","content":"two"}}),
        json!({"type":"session_end","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:04Z"}),
    ];
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write duplicate-ID transcript");
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let imported = fixture.run(&args);
    assert!(imported.status.success(), "{}", describe(&imported));
    let payload: Value = serde_json::from_slice(&imported.stdout).expect("import JSON");
    assert_eq!(payload["data"]["results"][0]["status"], "imported");
    assert_eq!(payload["data"]["results"][0]["checkpoints_written"], 2);
    assert_eq!(
        fixture
            .text_rows(
                "SELECT logical_turn_key FROM agent_coverage_claim ORDER BY created_at, logical_turn_key",
                "logical_turn_key"
            )
            .await,
        vec!["duplicate", "ordinal:1"],
        "the later duplicate must receive a deterministic collision-free key"
    );
    let replay = fixture.run(&args);
    assert!(replay.status.success(), "{}", describe(&replay));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        2,
        "duplicate-ID replay must remain idempotent"
    );
}

#[tokio::test]
async fn agent_import_activity_after_old_session_end_reopens_incomplete_session() {
    let fixture = ImportRepo::init();
    let session_id = "resumed-after-end";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let terminal_lines = [
        json!({"type":"user","uuid":"old-turn","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:00Z","message":{"role":"user","content":"old"}}),
        json!({"type":"assistant","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:01Z","message":{"role":"assistant","content":"done"}}),
        json!({"type":"session_end","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:00:02Z"}),
    ];
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            terminal_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write initially terminal transcript");
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let initial = fixture.run(&args);
    assert!(initial.status.success(), "{}", describe(&initial));
    assert_eq!(
        fixture
            .text_rows(
                "SELECT state FROM agent_session WHERE provider_session_id = 'resumed-after-end'",
                "state"
            )
            .await,
        vec!["stopped"],
        "the first import must establish the terminal lifecycle being reopened"
    );

    let mut resumed = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("open transcript for resumed activity");
    for line in [
        json!({"type":"user","uuid":"new-turn","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:01:00Z","message":{"role":"user","content":"resume"}}),
        json!({"type":"assistant","sessionId":session_id,"cwd":fixture.repo,
            "timestamp":"2026-07-15T01:01:01Z","message":{"role":"assistant","content":"still working"}}),
    ] {
        writeln!(resumed, "{line}").expect("append resumed activity");
    }
    drop(resumed);
    let imported = fixture.run(&args);
    assert!(imported.status.success(), "{}", describe(&imported));
    assert_eq!(
        fixture
            .text_rows(
                "SELECT state FROM agent_session WHERE provider_session_id = 'resumed-after-end'",
                "state"
            )
            .await,
        vec!["active"],
        "later semantic activity must clear an older terminal record"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_session
                 WHERE provider_session_id = 'resumed-after-end' AND stopped_at IS NULL"
            )
            .await,
        1,
        "a reopened active session must clear its obsolete stopped timestamp"
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT completeness FROM agent_coverage_claim WHERE logical_turn_key = 'new-turn'",
                "completeness"
            )
            .await,
        vec!["incomplete"],
        "the resumed tail remains upgradeable until newer terminal evidence"
    );
}

#[tokio::test]
async fn agent_import_enforces_configured_transcript_read_cap() {
    let fixture = ImportRepo::init();
    let configured = fixture.run(&["config", "set", "agent.max_transcript_read_bytes", "128"]);
    assert!(configured.status.success(), "{}", describe(&configured));
    let transcript = fixture.write_transcript("configured-cap", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success(), "oversized import passed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "unexpected failure: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
}

#[tokio::test]
async fn agent_import_diagnoses_config_above_adapter_hard_cap() {
    let fixture = ImportRepo::init();
    let configured = fixture.run(&[
        "config",
        "set",
        "agent.max_transcript_read_bytes",
        "33554432",
    ]);
    assert!(configured.status.success(), "{}", describe(&configured));
    let transcript = fixture.write_transcript("configured-hard-cap", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{}", describe(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("effective per-source cap is 16777216 bytes"),
        "configured hard-cap clamp was silent: {stderr}"
    );
}

#[tokio::test]
async fn agent_import_codex_rollout_e2e() {
    let fixture = ImportRepo::init();
    let session_id = "123e4567-e89b-12d3-a456-426614174000";
    let path = fixture
        .home
        .join(".codex/sessions/2026/07/15")
        .join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl"));
    std::fs::create_dir_all(path.parent().expect("rollout parent")).expect("create rollout dir");
    let lines = [
        json!({
            "type": "session_meta",
            "timestamp": "2026-07-15T01:00:00Z",
            "payload": {"id": session_id, "cwd": fixture.repo}
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-07-15T01:00:01Z",
            "payload": {"type": "message", "role": "user", "id": "turn-codex-1",
                "content": [{"type": "input_text", "text": "inspect"}]}
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-07-15T01:00:02Z",
            "payload": {"type": "message", "role": "assistant", "id": "reply-codex-1",
                "content": [{"type": "output_text", "text": "done"}]}
        }),
        json!({
            "type": "session_end",
            "timestamp": "2026-07-15T01:00:03Z",
            "payload": {"type": "session_end", "cwd": fixture.repo}
        }),
    ];
    std::fs::write(
        &path,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write rollout");

    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&path).as_str(),
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "codex import: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT source_channel FROM agent_coverage_claim",
                "source_channel"
            )
            .await,
        vec!["import"]
    );
}

#[tokio::test]
async fn agent_import_codex_malformed_arguments_upgrade_without_conflict() {
    let fixture = ImportRepo::init();
    let session_id = "123e4567-e89b-12d3-a456-426614174111";
    let path = fixture
        .home
        .join(".codex/sessions/2026/07/15")
        .join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl"));
    std::fs::create_dir_all(path.parent().expect("rollout parent")).expect("create rollout dir");
    let write_rollout = |arguments: Value| {
        let lines = [
            json!({
                "type": "session_meta", "timestamp": "2026-07-15T01:00:00Z",
                "payload": {"id": session_id, "cwd": fixture.repo}
            }),
            json!({
                "type": "response_item", "timestamp": "2026-07-15T01:00:01Z",
                "payload": {"type": "message", "role": "user", "content": "inspect"}
            }),
            json!({
                "type": "response_item", "timestamp": "2026-07-15T01:00:02Z",
                "payload": {"type": "function_call", "call_id": "call-1",
                    "name": "inspect", "arguments": arguments}
            }),
            json!({
                "type": "session_end", "timestamp": "2026-07-15T01:00:03Z",
                "payload": {"type": "session_end", "cwd": fixture.repo}
            }),
        ];
        std::fs::write(
            &path,
            format!(
                "{}\n",
                lines
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .expect("write rollout");
    };
    let path = path_arg(&path);
    let args = [
        "agent",
        "import",
        "--path",
        path.as_str(),
        "--agent",
        "codex",
        "--yes",
        "--json",
    ];

    write_rollout(Value::String("{\"path\":\"truncated".to_string()));
    let first = fixture.run(&args);
    assert!(
        first.status.success(),
        "malformed import: {}",
        describe(&first)
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT completeness FROM agent_coverage_claim",
                "completeness"
            )
            .await,
        vec!["incomplete"]
    );

    write_rollout(Value::String("{\"path\":\"Cargo.toml\"}".to_string()));
    let second = fixture.run(&args);
    assert!(
        second.status.success(),
        "corrected import: {}",
        describe(&second)
    );
    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open result db");
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT state, completeness FROM agent_coverage_claim".to_string(),
        ))
        .await
        .expect("query corrected claim")
        .expect("corrected claim");
    assert_eq!(
        row.try_get_by::<String, _>("state").unwrap(),
        "catalog_committed"
    );
    assert_eq!(
        row.try_get_by::<String, _>("completeness").unwrap(),
        "complete"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        2
    );
}

#[tokio::test]
async fn agent_import_reuses_same_repository_live_session_from_subdirectory() {
    let fixture = ImportRepo::init();
    let subdir = fixture.repo.join("nested/work");
    std::fs::create_dir_all(&subdir).expect("create repo subdir");
    let transcript = fixture.write_transcript("subdir123", &subdir, true);
    let repo_id = fixture.repo_id().await;
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url).await.expect("open repo db");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version,
            repo_id, worktree_id, workspace_id, workspace_fence, scope_state
         ) VALUES ('claude__subdir123', 'claude_code', 'subdir123', 'active', ?,
                   '{}', '{}', 1, 1, 1, ?, '', NULL, NULL, 'scoped')",
        [subdir.to_string_lossy().into_owned().into(), repo_id.into()],
    ))
    .await
    .expect("seed live session");
    conn.close().await.expect("close db");

    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "subdir import: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .text_rows("SELECT working_dir FROM agent_session", "working_dir")
            .await,
        vec![
            fixture
                .repo
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ]
    );
}

#[tokio::test]
async fn agent_import_preserves_newer_stopped_live_session_lifecycle() {
    let fixture = ImportRepo::init();
    let session_id = "stoppednewer";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let body = [
        json!({
            "type": "user",
            "uuid": "older-uncovered-turn",
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:00Z",
            "message": {"role": "user", "content": "import older hole"}
        }),
        json!({
            "type": "assistant",
            "uuid": "older-uncovered-answer",
            "sessionId": session_id,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:01Z",
            "message": {"role": "assistant", "content": "done"}
        }),
    ]
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(&transcript, format!("{body}\n")).expect("write nonterminal transcript");

    let newer_event_at = 9_999_999_999_000_i64;
    let newer_stopped_at = 9_999_999_999_500_i64;
    let repo_id = fixture.repo_id().await;
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url).await.expect("open repo db");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            stopped_at, schema_version, repo_id, worktree_id, workspace_id,
            workspace_fence, scope_state
         ) VALUES ('claude__stoppednewer', 'claude_code', ?, 'stopped', ?,
                   '{}', '{}', ?, ?, ?, 1, ?, '', NULL, NULL, 'scoped')",
        [
            session_id.into(),
            fixture.repo.to_string_lossy().into_owned().into(),
            newer_event_at.into(),
            newer_event_at.into(),
            newer_stopped_at.into(),
            repo_id.into(),
        ],
    ))
    .await
    .expect("seed newer stopped live session");
    conn.close().await.expect("close seed db");

    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "older-hole import: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1,
        "uncovered historical turn must still be imported"
    );

    let conn = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open result db");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT state, started_at, last_event_at, stopped_at FROM agent_session
             WHERE session_id = 'claude__stoppednewer'"
                .to_string(),
        ))
        .await
        .expect("query imported session lifecycle")
        .expect("session row remains");
    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert_eq!(
        row.try_get_by::<i64, _>("started_at").unwrap(),
        1_784_077_200,
        "historical import must merge the earliest observed session start"
    );
    assert_eq!(
        row.try_get_by::<i64, _>("last_event_at").unwrap(),
        newer_event_at
    );
    assert_eq!(
        row.try_get_by::<i64, _>("stopped_at").unwrap(),
        newer_stopped_at
    );
}

#[tokio::test]
async fn agent_import_updates_digest_without_changing_structural_parent() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("revision123", &fixture.repo, false);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run(&args);
    assert!(
        first.status.success(),
        "incomplete import: {}",
        describe(&first)
    );

    fixture.write_transcript("revision123", &fixture.repo, true);
    let second = fixture.run(&args);
    assert!(
        second.status.success(),
        "complete import: {}",
        describe(&second)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        2,
        "same logical turn keeps both source revisions"
    );
    let parents = fixture
        .text_rows(
            "SELECT COALESCE(parent_commit, '<root>') AS parent_commit \
             FROM agent_checkpoint ORDER BY created_at, checkpoint_id",
            "parent_commit",
        )
        .await;
    assert_eq!(parents.len(), 2);
    assert_eq!(
        parents[0], parents[1],
        "source revision must not alter the structural repository parent"
    );
}

#[tokio::test]
async fn agent_import_tombstone_blocks_resurrection_until_audited_restore() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("erase123", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let base_args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let imported = fixture.run(&base_args);
    assert!(
        imported.status.success(),
        "initial import: {}",
        describe(&imported)
    );
    let imported_fingerprint = fixture
        .text_rows(
            "SELECT json_extract(metadata_json, '$.source_fingerprint') AS fingerprint
             FROM agent_session WHERE session_id = 'claude__erase123'",
            "fingerprint",
        )
        .await
        .into_iter()
        .next()
        .expect("imported source fingerprint");
    let imported_sync_revision = fixture
        .scalar(
            "SELECT sync_revision AS n FROM agent_session WHERE session_id = 'claude__erase123'",
        )
        .await;

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local("claude__erase123")
        .await
        .expect("erase imported session");
    assert!(erased.session_deleted);
    drop(history);

    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_tombstone")
            .await,
        1
    );
    assert_eq!(
        fixture
            .text_rows(
                "SELECT source_fingerprint AS fingerprint FROM agent_import_tombstone",
                "fingerprint"
            )
            .await,
        vec![imported_fingerprint],
        "erasure must preserve the non-reversible import audit fingerprint"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_identity")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT next_session_sync_revision AS n FROM agent_capture_incarnation
                 WHERE agent_kind = 'claude_code' AND provider_session_id = 'erase123'",
            )
            .await,
        imported_sync_revision + 1,
        "erasure must preserve a strictly newer cloud replication epoch"
    );

    let blocked = fixture.run(&base_args);
    assert!(!blocked.status.success());
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("LBR-AGENT-019"),
        "{}",
        describe(&blocked)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );

    let restored = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--restore-erased",
        "--json",
    ]);
    assert!(
        restored.status.success(),
        "restored import: {}",
        describe(&restored)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_audit_log WHERE action = 'restore_erased_import'"
            )
            .await,
        1,
        "explicit restore is append-only audited"
    );
    assert!(
        fixture
            .scalar(
                "SELECT sync_revision AS n FROM agent_session
                 WHERE session_id = 'claude__erase123'",
            )
            .await
            > imported_sync_revision,
        "restored session must not reuse its erased cloud generation"
    );
    let incarnation = fixture
        .text_rows(
            "SELECT json_extract(metadata_json, '$.capture_incarnation') AS incarnation
             FROM agent_session WHERE session_id = 'claude__erase123'",
            "incarnation",
        )
        .await;
    assert_eq!(incarnation.len(), 1);
    assert_eq!(incarnation[0].len(), 32);
}

#[tokio::test]
async fn agent_import_restore_refuses_while_erasure_is_unfinished() {
    let fixture = ImportRepo::init();
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url).await.expect("open repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES ('claude__erasing', 'claude_code', 'erasing', 'stopped',
                   '/tmp', '{}', '{}', 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed erasing session");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_import_tombstone (
            tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
         ) VALUES ('t-erasing', 'claude_code', 'erasing', 'claude__erasing', 1)"
            .to_string(),
    ))
    .await
    .expect("seed tombstone");

    let error = restore_tombstone(&conn, AgentKind::ClaudeCode, "erasing")
        .await
        .expect_err("restore must wait for catalog deletion");
    assert!(error.to_string().contains("still being pruned"));
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_import_tombstone".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 1);
}

#[tokio::test]
async fn import_intermediate_erasure_tombstone_reaches_exact_fence_when_recheck_fails() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("erasure-midphase", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run(&args);
    assert!(first.status.success(), "{}", describe(&first));

    let barrier_ready = fixture._tmp.path().join("midphase-barrier-ready");
    let barrier_resume = fixture._tmp.path().join("midphase-barrier-resume");
    let replay = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_INDEX_BARRIER_READY_FILE", &barrier_ready)
        .env(
            "LIBRA_TEST_IMPORT_INDEX_BARRIER_CONTINUE_FILE",
            &barrier_resume,
        )
        .env("LIBRA_TEST_IMPORT_INDEX_TOMBSTONE_LOOKUP_FAIL", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("spawn replay at index barrier");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !barrier_ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        barrier_ready.exists(),
        "replay did not acquire index barrier"
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url).await.expect("open repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_import_tombstone (
            tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
         ) VALUES (
            't-erasure-midphase', 'claude_code', 'erasure-midphase',
            'claude__erasure-midphase', 1
         )"
        .to_string(),
    ))
    .await
    .expect("seed committed tombstone before catalog deletion");
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_session
                 WHERE session_id = 'claude__erasure-midphase'"
            )
            .await,
        1,
        "the regression requires erasure's tombstone-committed/catalog-retained window"
    );

    std::fs::write(&barrier_resume, b"continue").expect("resume tombstoned replay");
    let replay = replay
        .wait_with_output()
        .expect("wait for tombstoned replay");
    assert!(!replay.status.success(), "replay unexpectedly succeeded");
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("LBR-AGENT-019"),
        "the in-transaction exact fence must establish erasure even when the advisory recheck fails: {}",
        describe(&replay)
    );
}

#[tokio::test]
async fn agent_import_crash_recovery_never_double_appends() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("crash123", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];

    let before_objects = fixture.run_with_env(&args, "LIBRA_TEST_IMPORT_FAILPOINT", "after_bind");
    assert!(!before_objects.status.success());
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE state = 'reserved_import'"
            )
            .await,
        1
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_import_identity SET lease_expires_at = 0".to_string(),
    ))
    .await
    .expect("expire crashed identity lease");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim SET lease_expires_at = 0".to_string(),
    ))
    .await
    .expect("expire crashed coverage lease");
    drop(conn);

    let recovered = fixture.run(&args);
    assert!(
        recovered.status.success(),
        "recover bound attempt: {}",
        describe(&recovered)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );

    let committed_but_unreported = fixture.write_transcript("crash456", &fixture.repo, true);
    let committed_arg = path_arg(&committed_but_unreported);
    let committed_args = [
        "agent",
        "import",
        "--path",
        committed_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let failed_report = fixture.run_with_env(
        &committed_args,
        "LIBRA_TEST_IMPORT_FAILPOINT",
        "after_catalog_commit",
    );
    assert!(!failed_report.status.success());
    let failed_stderr = String::from_utf8_lossy(&failed_report.stderr);
    assert!(
        failed_stderr.contains("\"checkpoints_written\": 1"),
        "durable progress must be reported exactly: {}",
        describe(&failed_report)
    );
    assert!(
        failed_stderr.contains("\"succeeded\": 0")
            && failed_stderr.contains("\"partial\": 1")
            && failed_stderr.contains("\"partial_results\""),
        "one failed selection with durable progress must not be counted as a success: {}",
        describe(&failed_report)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        2
    );

    let replay = fixture.run(&committed_args);
    assert!(
        replay.status.success(),
        "replay committed attempt: {}",
        describe(&replay)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        2,
        "catalog-committed crash replay must not append a duplicate"
    );
}

#[tokio::test]
async fn agent_import_concurrent_recovery_appends_exactly_once() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("concurrent-recovery", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let ready = fixture._tmp.path().join("concurrent-import-ready");
    let resume = fixture._tmp.path().join("concurrent-import-resume");
    let first = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_BEFORE_BIND_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_BEFORE_BIND_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("spawn first importer");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "first importer did not reserve its claims");

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    for sql in [
        "UPDATE agent_import_identity SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE agent_coverage_claim SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE metadata_kv
         SET value = json_set(value, '$.lease_expires_at', 0)
         WHERE scope = 'agent_import_index_repair'",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("expire first importer lease");
    }
    drop(conn);

    let recovered = fixture.run(&args);
    assert!(
        recovered.status.success(),
        "takeover importer: {}",
        describe(&recovered)
    );
    std::fs::write(&resume, b"continue").expect("resume fenced importer");
    let stale = first.wait_with_output().expect("wait for fenced importer");
    assert!(
        !stale.status.success(),
        "fenced importer unexpectedly reported success: {}",
        describe(&stale)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1,
        "concurrent takeover must append one checkpoint"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
            .await,
        1,
        "concurrent takeover must publish one coverage revision"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity
                 WHERE state = 'committed' AND owner IS NULL"
            )
            .await,
        1,
        "the winning importer must finalize the shared identity"
    );
}

#[tokio::test]
async fn concurrent_import_loser_cannot_clear_winners_index_barrier() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("index-barrier-race", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let ready = fixture._tmp.path().join("index-barrier-ready");
    let resume = fixture._tmp.path().join("index-barrier-resume");
    let first = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_INDEX_BARRIER_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_INDEX_BARRIER_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("spawn index-barrier owner");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready.exists(),
        "first importer did not acquire index barrier"
    );
    let before = fixture
        .text_rows(
            "SELECT value FROM metadata_kv
             WHERE scope = 'agent_import_index_repair'
               AND target = 'claude__index-barrier-race'",
            "value",
        )
        .await;
    assert_eq!(before.len(), 1);

    let loser = fixture.run(&args);
    assert!(
        !loser.status.success(),
        "losing importer: {}",
        describe(&loser)
    );
    let after = fixture
        .text_rows(
            "SELECT value FROM metadata_kv
             WHERE scope = 'agent_import_index_repair'
               AND target = 'claude__index-barrier-race'",
            "value",
        )
        .await;
    assert_eq!(
        after, before,
        "a lease-losing process must not overwrite or retire the active writer's barrier"
    );

    std::fs::write(&resume, b"continue").expect("resume index-barrier owner");
    let winner = first
        .wait_with_output()
        .expect("wait for index-barrier owner");
    assert!(
        winner.status.success(),
        "winning importer: {}",
        describe(&winner)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-barrier-race'"
            )
            .await,
        0,
        "only the owning generation may retire the completed barrier"
    );
}

#[tokio::test]
async fn crashed_provisional_session_is_reaped_after_takeover_fails_without_progress() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("provisional-crash", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let crashed = fixture.run_with_env(&args, "LIBRA_TEST_IMPORT_FAILPOINT", "after_bind");
    assert!(!crashed.status.success(), "crash failpoint did not fire");

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_import_identity SET lease_expires_at = 0".to_string(),
    ))
    .await
    .expect("expire crashed identity lease");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim SET lease_expires_at = 0".to_string(),
    ))
    .await
    .expect("expire crashed coverage lease");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TRIGGER reject_recovered_marker
         BEFORE UPDATE OF value ON metadata_kv
         WHEN OLD.scope = 'agent_traces_inflight'
         BEGIN
             SELECT RAISE(ABORT, 'test recovered marker failure');
         END"
        .to_string(),
    ))
    .await
    .expect("reject takeover marker");
    drop(conn);

    let retry = fixture.run(&args);
    assert!(
        !retry.status.success(),
        "failing takeover unexpectedly passed"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "a recovered zero-progress provisional session became permanent"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "failed takeover left its ordinary writer marker"
    );
}

#[tokio::test]
async fn crashed_import_takeover_abandons_claims_missing_from_shrunk_source() {
    let fixture = ImportRepo::init();
    let session_id = "shrunk-after-crash";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let turn = |ordinal: usize| {
        [
            json!({
                "type": "user", "uuid": format!("shrunk-turn-{ordinal}"),
                "sessionId": session_id, "cwd": fixture.repo,
                "timestamp": format!("2026-07-15T01:0{ordinal}:00Z"),
                "message": {"role": "user", "content": format!("question {ordinal}")}
            }),
            json!({
                "type": "assistant", "uuid": format!("shrunk-answer-{ordinal}"),
                "sessionId": session_id, "cwd": fixture.repo,
                "timestamp": format!("2026-07-15T01:0{ordinal}:01Z"),
                "message": {"role": "assistant", "content": format!("answer {ordinal}")}
            }),
        ]
    };
    let first_turn = turn(0);
    let second_turn = turn(1);
    let full = first_turn
        .iter()
        .chain(second_turn.iter())
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&transcript, format!("{full}\n")).expect("write two-turn transcript");
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let crashed = fixture
        .command()
        .env("LIBRA_TEST_CHECKPOINT_CRASH_AFTER_FIRST_OBJECT", "1")
        .args(args)
        .output()
        .expect("run crashed importer");
    assert_eq!(crashed.status.code(), Some(86), "{}", describe(&crashed));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'reserved_import'"
            )
            .await,
        2,
        "crash fixture must reserve both source turns before object construction"
    );

    let conn = Database::connect(format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open writable repo db");
    for sql in [
        "UPDATE agent_import_identity SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE agent_coverage_claim SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE metadata_kv
         SET value = json_set(value, '$.lease_expires_at', 0)
         WHERE scope = 'agent_import_index_repair'",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("expire crashed lease");
    }
    conn.close().await.expect("close writable db");

    let shrunk = first_turn
        .iter()
        .map(Value::to_string)
        .chain(std::iter::once(
            json!({
                "type": "session_end", "sessionId": session_id, "cwd": fixture.repo,
                "timestamp": "2026-07-15T01:00:02Z"
            })
            .to_string(),
        ))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&transcript, format!("{shrunk}\n")).expect("shrink transcript");
    let recovered = fixture.run(&args);
    assert!(
        recovered.status.success(),
        "takeover: {}",
        describe(&recovered)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE state = 'reserved_import' OR owner IS NOT NULL"
            )
            .await,
        0,
        "takeover must not strand the missing turn under the crashed owner"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'abandoned'")
            .await,
        1,
        "the removed turn must remain explicitly abandoned and fenced"
    );
}

#[tokio::test]
async fn crash_after_first_object_is_recovered_from_durable_attempt_ownership() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("object-crash", &fixture.repo, true);
    let objects_dir = fixture.repo.join(".libra/objects");
    let objects_before = loose_object_file_count(&objects_dir);
    let output = fixture
        .command()
        .env("LIBRA_TEST_CHECKPOINT_CRASH_AFTER_FIRST_OBJECT", "1")
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run object-construction crash probe");
    assert_eq!(output.status.code(), Some(86), "{}", describe(&output));
    assert!(
        loose_object_file_count(&objects_dir) > objects_before,
        "crash probe did not leave the first loose object"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE metadata_kv
         SET value = json_set(value, '$.started_at_ms', 0, '$.ttl_ms', 0)
         WHERE scope = 'agent_traces_inflight'"
            .to_string(),
    ))
    .await
    .expect("expire crashed object owner marker");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    history
        .erase_session_local("claude__object-crash")
        .await
        .expect("erasure should not block on unrelated object reclamation");
    assert!(
        loose_object_file_count(&objects_dir) > objects_before,
        "expired crash ownership must leave physical reclamation to repository GC"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        1,
        "foreground erasure must leave full reachability cleanup to doctor/GC"
    );
    let repaired = fixture.run(&["agent", "doctor", "--repair", "--json"]);
    assert!(repaired.status.success(), "{}", describe(&repaired));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "doctor repair must retire the expired ownership marker"
    );
}

#[tokio::test]
async fn expired_object_crash_takeover_retires_marker_and_resumes_import() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("object-takeover", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let crashed = fixture
        .command()
        .env("LIBRA_TEST_CHECKPOINT_CRASH_AFTER_FIRST_OBJECT", "1")
        .args(args)
        .output()
        .expect("run object crash probe");
    assert_eq!(crashed.status.code(), Some(86), "{}", describe(&crashed));
    let marker_values = fixture
        .text_rows(
            "SELECT value FROM metadata_kv WHERE scope = 'agent_traces_inflight'",
            "value",
        )
        .await;
    assert_eq!(marker_values.len(), 1);
    let marker: Value = serde_json::from_str(&marker_values[0]).expect("parse crash marker");
    assert!(
        marker["created_oids"]
            .as_array()
            .is_some_and(|oids| !oids.is_empty()),
        "crash marker did not persist proven-created object ownership: {marker}"
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    for sql in [
        "UPDATE agent_import_identity SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE agent_coverage_claim SET lease_expires_at = 0 WHERE owner IS NOT NULL",
        "UPDATE metadata_kv
         SET value = json_set(value, '$.started_at_ms', 0, '$.ttl_ms', 0)
         WHERE scope = 'agent_traces_inflight'",
        "UPDATE metadata_kv
         SET value = json_set(value, '$.lease_expires_at', 0)
         WHERE scope = 'agent_import_index_repair'",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("expire crashed import lease or marker");
    }
    drop(conn);

    let resumed = fixture.run(&args);
    assert!(
        resumed.status.success(),
        "expired crash takeover failed: {}",
        describe(&resumed)
    );
    assert!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await
            > 0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        1,
        "successful takeover must not run a full all-ref cleanup drain on the append hot path"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state = 'committed' AND owner IS NULL"
            )
            .await,
        1
    );
    let repaired = fixture.run(&["agent", "doctor", "--repair", "--json"]);
    assert!(repaired.status.success(), "{}", describe(&repaired));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "doctor repair must retire the superseded writer marker"
    );
}

#[tokio::test]
async fn agent_import_failure_advances_only_source_ordinal_prefix() {
    let fixture = ImportRepo::init();
    let path = fixture.transcript_path("ordinal123");
    std::fs::create_dir_all(path.parent().expect("transcript parent"))
        .expect("create transcript dir");
    let lines = [
        json!({
            "type": "user", "uuid": "z-turn", "sessionId": "ordinal123",
            "cwd": fixture.repo, "message": {"role": "user", "content": "first"}
        }),
        json!({
            "type": "assistant", "uuid": "z-answer", "sessionId": "ordinal123",
            "cwd": fixture.repo, "message": {"role": "assistant", "content": "one"}
        }),
        json!({
            "type": "user", "uuid": "a-turn", "sessionId": "ordinal123",
            "cwd": fixture.repo, "message": {"role": "user", "content": "second"}
        }),
        json!({
            "type": "assistant", "uuid": "a-answer", "sessionId": "ordinal123",
            "cwd": fixture.repo, "message": {"role": "assistant", "content": "two"}
        }),
        json!({
            "type": "session_end", "sessionId": "ordinal123", "cwd": fixture.repo
        }),
    ];
    std::fs::write(
        &path,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write adversarial-order transcript");
    let output = fixture.run_with_env(
        &[
            "agent",
            "import",
            "--path",
            path_arg(&path).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ],
        "LIBRA_TEST_IMPORT_FAILPOINT",
        "after_catalog_commit",
    );
    assert!(!output.status.success(), "failpoint did not fire");
    assert_eq!(
        fixture
            .text_rows(
                "SELECT logical_turn_key FROM agent_coverage_claim \
                 WHERE state = 'catalog_committed'",
                "logical_turn_key",
            )
            .await,
        vec!["z-turn"],
        "provider-key sorting must not commit source ordinal 1 before ordinal 0"
    );
    assert_eq!(
        fixture
            .scalar("SELECT next_ordinal AS n FROM agent_import_identity")
            .await,
        1,
        "partial finalization must preserve the committed contiguous prefix"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'reserved_import'"
            )
            .await,
        0,
        "terminal partial progress must release every unused reservation immediately"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "terminal partial progress must not retain a non-cleanup marker"
    );
}

/// ADR-DR-19 erasure-window matrix, import half: the `after_bind` probe stops
/// after the identity and turn reservation transactions but before object
/// construction. Erasing at that point must remove/fence every durable holder;
/// therefore the same stale attempt cannot reach either the object-construction
/// or final ref/catalog transaction windows, and a fresh process is rejected
/// before it can recreate the session. The complementary object-built/final
/// transaction interleaving is pinned by
/// `coverage_gate::tests::tombstone_blocks_reservation_and_fences_reserved_commit`.
#[tokio::test]
async fn agent_erase_between_reservation_and_objects_blocks_all_later_import_windows() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("erasewindow123", &fixture.repo, true);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];

    let stopped = fixture.run_with_env(&args, "LIBRA_TEST_IMPORT_FAILPOINT", "after_bind");
    assert!(
        !stopped.status.success(),
        "failpoint did not stop: {}",
        describe(&stopped)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_identity WHERE state = 'writing'")
            .await,
        1,
        "identity lease must be durable before the erasure interleaving"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'reserved_import'"
            )
            .await,
        1,
        "turn reservation must be durable before the erasure interleaving"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0,
        "the probe is before object/ref/catalog publication"
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let blocked = history
        .erase_session_local("claude__erasewindow123")
        .await
        .expect_err("live pre-object marker must block an apparently empty erasure");
    assert!(
        format!("{blocked:#}").contains("in-flight"),
        "unexpected erasure refusal: {blocked:#}"
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE metadata_kv
             SET value = json_set(value, '$.started_at_ms', 0, '$.ttl_ms', 0)
             WHERE scope = 'agent_traces_inflight'
               AND target = 'claude__erasewindow123'"
            .to_string(),
    ))
    .await
    .expect("expire crashed writer marker");
    let erased = history
        .erase_session_local("claude__erasewindow123")
        .await
        .expect("erase session after crashed marker expiry");
    assert!(erased.session_deleted);
    drop(history);

    for table in [
        "agent_session",
        "agent_checkpoint",
        "agent_coverage_claim",
        "agent_coverage_revision",
        "agent_import_identity",
        "agent_export_job",
    ] {
        assert_eq!(
            fixture
                .scalar(&format!("SELECT COUNT(*) AS n FROM {table}"))
                .await,
            0,
            "erasure left reachable or resumable state in {table}"
        );
    }
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_tombstone")
            .await,
        1
    );

    let replay = fixture.run(&args);
    assert!(!replay.status.success(), "erased import replay succeeded");
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("LBR-AGENT-019"),
        "{}",
        describe(&replay)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "post-erase parsing must not recreate the session"
    );
}

#[tokio::test]
async fn concurrent_erase_blocks_marked_writer_and_leaves_rejected_objects_for_gc() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("eraserace123", &fixture.repo, true);
    let ready = fixture._tmp.path().join("import-ready");
    let resume = fixture._tmp.path().join("import-resume");
    let mut command = fixture.command();
    let child = command
        .env("LIBRA_TEST_IMPORT_AFTER_BIND_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_AFTER_BIND_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .spawn()
        .expect("spawn paused importer");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready.exists(),
        "importer did not reach the bound-attempt window"
    );

    let objects_dir = fixture.repo.join(".libra/objects");
    let objects_before = loose_object_file_count(&objects_dir);
    let index_before = fixture
        .scalar("SELECT COUNT(*) AS n FROM object_index")
        .await;
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let blocked = history
        .erase_session_local("claude__eraserace123")
        .await
        .expect_err("erase must not report success while its writer marker is live");
    assert!(format!("{blocked:#}").contains("in-flight"));

    std::fs::write(&resume, b"continue").expect("resume stale importer");
    let output = child.wait_with_output().expect("wait for stale importer");
    assert!(
        !output.status.success(),
        "tombstoned stale importer unexpectedly committed: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv \
                 WHERE scope = 'agent_traces_inflight' \
                   AND target = 'claude__eraserace123'"
            )
            .await,
        1,
        "rejected append must leave durable ownership for doctor/GC: {}",
        describe(&output)
    );
    assert!(
        loose_object_file_count(&objects_dir) > objects_before,
        "rejected objects were unsafely unlinked instead of being left for repository GC"
    );
    assert!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM object_index")
            .await
            >= index_before,
        "inline cleanup must not delete shared object_index rows"
    );

    let still_blocked = history
        .erase_session_local("claude__eraserace123")
        .await
        .expect_err("fresh durable cleanup ownership must still block erasure");
    assert!(
        format!("{still_blocked:#}").contains("in-flight"),
        "unexpected cleanup-pending erasure refusal: {still_blocked:#}"
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE metadata_kv
         SET value = json_set(value, '$.started_at_ms', 0, '$.ttl_ms', 0)
         WHERE scope = 'agent_traces_inflight'
           AND target = 'claude__eraserace123'"
            .to_string(),
    ))
    .await
    .expect("expire rejected cleanup marker");
    let expired_still_blocked = history
        .erase_session_local("claude__eraserace123")
        .await
        .expect_err("expired durable cleanup ownership must still block erasure");
    assert!(
        format!("{expired_still_blocked:#}").contains("in-flight"),
        "unexpected expired-cleanup erasure refusal: {expired_still_blocked:#}"
    );
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE metadata_kv
         SET value = json_set(value, '$.started_at_ms', ?, '$.ttl_ms', 120000)
         WHERE scope = 'agent_traces_inflight'
           AND target = 'claude__eraserace123'",
        [chrono::Utc::now().timestamp_millis().into()],
    ))
    .await
    .expect("refresh cleanup marker TTL before doctor repair");
    drop(history);
    drop(conn);
    let repaired = fixture.run(&["agent", "doctor", "--repair", "--json"]);
    assert!(repaired.status.success(), "{}", describe(&repaired));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'
                   AND target = 'claude__eraserace123'"
            )
            .await,
        0,
        "doctor must retire the durable rejected-object ownership marker"
    );

    let conn = Database::connect(format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("reopen writable repo db");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(conn),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local("claude__eraserace123")
        .await
        .expect("retry erasure after doctor retires rejected ownership");
    assert!(
        erased.session_deleted,
        "a fenced stale importer must leave provisional-session deletion to the erasure owner"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_tombstone")
            .await,
        1,
        "idempotent retry lost the anti-resurrection barrier"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_doctor_retires_marker_without_reading_fifo_cleanup_root() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt as _, fs::FileTypeExt as _},
    };

    let fixture = ImportRepo::init();
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable FIFO cleanup database");
    let libra_dir = fixture.repo.join(".libra");
    let fifo_oid = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
        .expect("valid FIFO cleanup oid");
    let fifo_text = fifo_oid.to_string();
    let fifo_shard = libra_dir.join("objects").join(&fifo_text[..2]);
    std::fs::create_dir_all(&fifo_shard).expect("create FIFO cleanup shard");
    let fifo = fifo_shard.join(&fifo_text[2..]);
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    // SAFETY: fifo_name is NUL-terminated and lies in this test's temporary
    // repository.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
         VALUES ('fifo-cleanup-root', 'Branch', ?, NULL, NULL)",
        [fifo_text.clone().into()],
    ))
    .await
    .expect("register FIFO only as a cleanup graph root");
    let mut marker = TracesInflightMarker::new(
        "claude__fifo-cleanup",
        "fifo-cleanup-attempt",
        chrono::Utc::now().timestamp_millis(),
    );
    marker.cleanup_pending = true;
    marker.oids.push(fifo_text.clone());
    marker.created_oids.push(fifo_text);
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("seed FIFO cleanup ownership");
    conn.close().await.expect("close FIFO cleanup database");

    let mut child = fixture
        .command()
        .args(["agent", "doctor", "--repair", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn FIFO cleanup doctor");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child
            .try_wait()
            .expect("poll FIFO cleanup doctor")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill hung FIFO cleanup doctor");
            let output = child.wait_with_output().expect("reap hung FIFO doctor");
            panic!(
                "doctor hung on a special-file loose object: {}",
                describe(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("collect FIFO cleanup doctor");
    assert!(output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("root-fenced ownership retirement; repository GC owns payload reachability"),
        "doctor did not report non-destructive ownership retirement: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "successful ownership retirement left the cleanup marker behind"
    );
    assert!(
        fifo.symlink_metadata()
            .expect("inspect FIFO after marker retirement")
            .file_type()
            .is_fifo(),
        "non-destructive marker retirement replaced or removed the FIFO payload"
    );
}

#[tokio::test]
async fn agent_doctor_repair_kills_index_snapshot_helper_at_aggregate_deadline() {
    let fixture = ImportRepo::init();
    let conn = Database::connect(format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open writable cleanup database");
    let mut marker = TracesInflightMarker::new(
        "claude__hung-index-snapshot",
        "hung-index-snapshot-attempt",
        chrono::Utc::now().timestamp_millis(),
    );
    marker.cleanup_pending = true;
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("seed cleanup ownership");
    conn.close().await.expect("close cleanup database");

    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_REJECTED_CLEANUP_DEADLINE_MS", "100")
        .env("LIBRA_TEST_REJECTED_CLEANUP_INDEX_HELPER_DELAY_MS", "5000")
        .args(["agent", "doctor", "--repair", "--json"])
        .output()
        .expect("run bounded cleanup doctor");
    assert!(output.status.success(), "{}", describe(&output));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "doctor did not kill its blocked filesystem helper: {}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("traversal deadline"),
        "deadline failure was not actionable: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        1,
        "failed root proof must retain durable cleanup ownership"
    );
}

#[test]
fn agent_import_non_tty_requires_yes_before_content_read() {
    let fixture = ImportRepo::init();
    let nonexistent = fixture.transcript_path("missing");
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&nonexistent).as_str(),
        "--agent",
        "claude-code",
        "--json",
    ]);
    assert!(!output.status.success(), "import unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{}", describe(&output));
    assert!(stderr.contains("--yes"), "{}", describe(&output));
    assert!(
        !stderr.contains("source authorization"),
        "source must not be inspected before consent: {}",
        describe(&output)
    );
}

#[test]
fn agent_import_confirmation_precedes_opencode_export() {
    let fixture = ImportRepo::init();
    let output = fixture.run(&[
        "agent",
        "import",
        "--session",
        "opencode123",
        "--agent",
        "opencode",
        "--json",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{}", describe(&output));
    assert!(stderr.contains("--yes"), "{}", describe(&output));
    assert!(
        !stderr.contains("trusted") && !stderr.contains("exporter"),
        "export capability must not be probed before consent: {}",
        describe(&output)
    );
}

#[test]
fn agent_import_rejects_cross_repo_and_ambiguous_working_dir() {
    let fixture = ImportRepo::init();
    let other = fixture._tmp.path().join("other");
    std::fs::create_dir_all(&other).expect("create other repo");
    let mut init = Command::new(env!("CARGO_BIN_EXE_libra"));
    let init_output = init
        .current_dir(&other)
        .env("HOME", &fixture.home)
        .env("LIBRA_TEST_HOME", &fixture.home)
        .arg("init")
        .output()
        .expect("init other repo");
    assert!(
        init_output.status.success(),
        "other init: {}",
        describe(&init_output)
    );

    let cross = fixture.write_transcript("cross123", &other, true);
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&cross).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-015"),
        "{}",
        describe(&output)
    );

    let ambiguous = fixture.write_transcript("ambiguous123", &fixture.repo, true);
    let second = json!({
        "type": "assistant",
        "uuid": "assistant-2",
        "sessionId": "ambiguous123",
        "cwd": other,
        "message": {"role": "assistant", "content": "different repo"}
    });
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&ambiguous)
            .expect("open transcript"),
        "{second}"
    )
    .expect("append ambiguous cwd");
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&ambiguous).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-016"),
        "{}",
        describe(&output)
    );
}

#[tokio::test]
async fn agent_import_accepts_sibling_linked_worktree_of_same_repository() {
    let fixture = ImportRepo::init();
    let sibling = fixture._tmp.path().join("sibling-worktree");
    let sibling_arg = path_arg(&sibling);
    let added = fixture.run(&["worktree", "add", sibling_arg.as_str()]);
    assert!(
        added.status.success(),
        "create sibling linked worktree: {}",
        describe(&added)
    );
    let transcript = fixture.write_transcript("siblingwt123", &sibling, true);
    let imported = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        imported.status.success(),
        "same-storage sibling worktree was rejected: {}",
        describe(&imported)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );
}

#[test]
fn agent_import_rejects_missing_working_dir_and_root_escape() {
    let fixture = ImportRepo::init();
    let missing = fixture.transcript_path("missingcwd123");
    std::fs::create_dir_all(missing.parent().expect("transcript parent"))
        .expect("create transcript dir");
    std::fs::write(
        &missing,
        json!({
            "type": "user",
            "uuid": "turn-1",
            "sessionId": "missingcwd123",
            "message": {"role": "user", "content": "no cwd"}
        })
        .to_string(),
    )
    .expect("write missing-cwd transcript");
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&missing).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-016"),
        "{}",
        describe(&output)
    );

    let outside = fixture._tmp.path().join("outside123.jsonl");
    std::fs::write(&outside, "not authorized").expect("write outside source");
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&outside).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "{}",
        describe(&output)
    );
}

#[test]
fn agent_import_rejects_filename_only_provider_identity() {
    let fixture = ImportRepo::init();
    let path = fixture.transcript_path("filenameonly123");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "turn-1",
                "cwd": fixture.repo,
                "message": {"role": "user", "content": "missing provider id"}
            })
        ),
    )
    .unwrap();
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&path).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-015"),
        "{}",
        describe(&output)
    );
}

#[cfg(unix)]
#[test]
fn agent_import_codex_discovery_rejects_symlinked_sessions_root_pre_consent() {
    let fixture = ImportRepo::init();
    let codex_home = fixture._tmp.path().join("codex-home");
    let outside = fixture._tmp.path().join("outside-sessions");
    let session_id = "123e4567-e89b-12d3-a456-426614174099";
    let day = outside.join("2026/07/15");
    std::fs::create_dir_all(&day).expect("create outside rollout directory");
    std::fs::create_dir_all(&codex_home).expect("create Codex home");
    let rollout = day.join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl"));
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            json!({
                "type": "session_meta",
                "timestamp": "2026-07-15T01:00:00Z",
                "payload": {"id": session_id, "cwd": fixture.repo}
            })
        ),
    )
    .expect("write outside rollout");
    std::os::unix::fs::symlink(&outside, codex_home.join("sessions"))
        .expect("symlink sessions root");

    for selector in [vec!["--session", session_id], vec!["--all"]] {
        let mut command = fixture.command();
        command
            .env("CODEX_HOME", &codex_home)
            .args(["agent", "import"])
            .args(selector)
            .args(["--agent", "codex", "--yes", "--json"]);
        let output = command.output().expect("run Codex discovery import");
        assert!(!output.status.success(), "{}", describe(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
            "unexpected failure: {}",
            describe(&output)
        );
    }
    assert!(rollout.exists(), "outside rollout must remain untouched");
}

#[cfg(unix)]
#[test]
fn agent_import_codex_discovery_rejects_symlinked_nested_directory_pre_consent() {
    let fixture = ImportRepo::init();
    let codex_home = fixture._tmp.path().join("codex-home-nested");
    let sessions = codex_home.join("sessions");
    let outside_month = fixture._tmp.path().join("outside-month");
    let session_id = "123e4567-e89b-12d3-a456-426614174098";
    std::fs::create_dir_all(sessions.join("2026")).expect("create sessions year");
    std::fs::create_dir_all(outside_month.join("15")).expect("create outside month");
    let victim = outside_month
        .join("15")
        .join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl"));
    std::fs::write(&victim, b"outside").expect("write outside victim");
    std::os::unix::fs::symlink(&outside_month, sessions.join("2026/07"))
        .expect("symlink month component");

    for selector in [vec!["--all"], vec!["--session", session_id]] {
        let output = fixture
            .command()
            .env("CODEX_HOME", &codex_home)
            .args(["agent", "import"])
            .args(selector)
            .args(["--agent", "codex", "--yes", "--json"])
            .output()
            .expect("run nested Codex discovery");
        assert!(!output.status.success(), "{}", describe(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
            "{}",
            describe(&output)
        );
    }
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn agent_import_codex_session_discovery_rejects_component_swap_pre_consent() {
    let fixture = ImportRepo::init();
    let codex_home = fixture._tmp.path().join("codex-home-swap");
    let sessions = codex_home.join("sessions");
    let original_year = sessions.join("2026");
    let original_day = original_year.join("07/15");
    let outside_year = fixture._tmp.path().join("outside-year-swap");
    let outside_day = outside_year.join("07/15");
    let session_id = "123e4567-e89b-12d3-a456-426614174097";
    std::fs::create_dir_all(&original_day).expect("create original Codex date tree");
    std::fs::create_dir_all(&outside_day).expect("create outside Codex date tree");
    for day in [&original_day, &outside_day] {
        std::fs::write(
            day.join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl")),
            b"outside must not be read",
        )
        .expect("write rollout swap fixture");
    }
    let ready = fixture._tmp.path().join("codex-open-ready");
    let resume = fixture._tmp.path().join("codex-open-resume");
    let child = fixture
        .command()
        .env("CODEX_HOME", &codex_home)
        .env("LIBRA_TEST_CODEX_DISCOVERY_OPEN_READY_FILE", &ready)
        .env("LIBRA_TEST_CODEX_DISCOVERY_OPEN_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "agent",
            "import",
            "--session",
            session_id,
            "--agent",
            "codex",
            "--yes",
            "--json",
        ])
        .spawn()
        .expect("spawn paused Codex session discovery");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "Codex finder did not reach its pinned open");

    let moved_year = sessions.join("2026-original");
    std::fs::rename(&original_year, &moved_year).expect("move checked Codex year");
    std::os::unix::fs::symlink(&outside_year, &original_year)
        .expect("replace checked year with outside symlink");
    std::fs::write(&resume, b"continue").expect("resume Codex discovery open");
    let output = child.wait_with_output().expect("wait for Codex discovery");
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "component replacement must fail closed: {}",
        describe(&output)
    );
    assert!(
        outside_day
            .join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl"))
            .exists(),
        "outside rollout must remain untouched"
    );
}

#[test]
fn agent_import_codex_discovery_fanout_limit_fails_loudly() {
    let fixture = ImportRepo::init();
    let codex_home = fixture._tmp.path().join("codex-home-fanout");
    let sessions = codex_home.join("sessions");
    std::fs::create_dir_all(&sessions).expect("create Codex sessions root");
    for index in 0..=20_000 {
        std::fs::create_dir(sessions.join(format!("junk-{index:05}")))
            .expect("create Codex fanout entry");
    }

    let output = fixture
        .command()
        .env("CODEX_HOME", &codex_home)
        .args([
            "agent", "import", "--all", "--agent", "codex", "--yes", "--json",
        ])
        .output()
        .expect("run bounded Codex discovery");
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "the discovery safety bound must fail loudly: {}",
        describe(&output)
    );
}

#[test]
fn agent_import_absolute_deadline_kills_blocked_discovery_helper() {
    let fixture = ImportRepo::init();
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "150")
        .env("LIBRA_TEST_IMPORT_DISCOVERY_HELPER_DELAY_MS", "2000")
        .args([
            "agent",
            "import",
            "--all",
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run blocked discovery helper");
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "killable discovery helper exceeded the absolute command deadline: {}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
}

#[cfg(unix)]
#[test]
fn agent_import_claude_discovery_rejects_symlinked_source_pre_consent() {
    let fixture = ImportRepo::init();
    let discovered = fixture.discoverable_transcript_path("symlink-source");
    std::fs::create_dir_all(discovered.parent().expect("discovery parent"))
        .expect("create Claude discovery directory");
    let victim = fixture._tmp.path().join("outside-claude.jsonl");
    std::fs::write(&victim, b"outside").expect("write outside Claude source");
    std::os::unix::fs::symlink(&victim, &discovered).expect("symlink Claude source");

    let output = fixture.run(&[
        "agent",
        "import",
        "--all",
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "{}",
        describe(&output)
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside");
}

#[tokio::test]
async fn agent_import_batch_limit_cursor_filter_and_partial_contract() {
    let fixture = ImportRepo::init();
    for session_id in ["batch001", "batch002", "batch003"] {
        fixture.write_discoverable_transcript(session_id, &fixture.repo);
    }
    let first = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-07-01T00:00:00Z",
        "--agent",
        "claude-code",
        "--limit",
        "2",
        "--yes",
        "--json",
    ]);
    assert!(first.status.success(), "first page: {}", describe(&first));
    let first_json: Value = serde_json::from_slice(&first.stdout).expect("first page JSON");
    assert_eq!(
        first_json["data"]["results"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(first_json["data"]["next_cursor"], 2);

    let second = fixture.run(&[
        "agent",
        "import",
        "--all",
        "--agent",
        "claude-code",
        "--limit",
        "2",
        "--cursor",
        "2",
        "--yes",
        "--json",
    ]);
    assert!(
        second.status.success(),
        "second page: {}",
        describe(&second)
    );
    let second_json: Value = serde_json::from_slice(&second.stdout).expect("second page JSON");
    assert_eq!(
        second_json["data"]["results"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(second_json["data"]["next_cursor"].is_null());
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        3
    );

    let invalid = fixture.write_discoverable_transcript("batchbad", &fixture.repo);
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&invalid)
            .expect("open invalid batch transcript"),
        "{}",
        json!({
            "type": "assistant",
            "sessionId": "batchbad",
            "cwd": fixture._tmp.path().join("not-a-repo"),
            "message": {"role": "assistant", "content": "ambiguous cwd"}
        })
    )
    .expect("append invalid batch cwd");
    fixture.write_discoverable_transcript("batchgood", &fixture.repo);
    let partial = fixture.run(&[
        "agent",
        "import",
        "--all",
        "--agent",
        "claude-code",
        "--limit",
        "50",
        "--yes",
        "--json",
    ]);
    assert!(!partial.status.success());
    let stderr = String::from_utf8_lossy(&partial.stderr);
    assert!(stderr.contains("LBR-AGENT-018"), "{}", describe(&partial));
    assert!(stderr.contains("LBR-AGENT-016"), "{}", describe(&partial));
    assert!(stderr.contains("\"results\""), "{}", describe(&partial));
    assert!(
        stderr.contains("\"schema_version\": 1"),
        "{}",
        describe(&partial)
    );
    assert!(
        stderr.contains("\"next_cursor\": null"),
        "{}",
        describe(&partial)
    );
    assert!(stderr.contains("sha256:"), "{}", describe(&partial));
    assert!(!stderr.contains("\"session_id\": \"partial\""));
}

#[tokio::test]
async fn agent_import_codex_batch_reports_cross_repository_candidates_as_skipped() {
    let fixture = ImportRepo::init();
    let other = fixture._tmp.path().join("other-repo");
    std::fs::create_dir_all(&other).expect("create other repo");
    let init = fixture
        .command()
        .current_dir(&other)
        .arg("init")
        .output()
        .expect("init other repo");
    assert!(init.status.success(), "{}", describe(&init));

    let codex_home = fixture._tmp.path().join("codex-home");
    let day = codex_home.join("sessions/2026/07/15");
    std::fs::create_dir_all(&day).expect("create Codex day partition");
    for (session_id, cwd, minute) in [
        (
            "123e4567-e89b-12d3-a456-426614174010",
            fixture.repo.as_path(),
            "00",
        ),
        (
            "123e4567-e89b-12d3-a456-426614174011",
            other.as_path(),
            "01",
        ),
    ] {
        let lines = [
            json!({
                "type": "session_meta",
                "timestamp": format!("2026-07-15T01:{minute}:00Z"),
                "payload": {"id": session_id, "cwd": cwd}
            }),
            json!({
                "type": "response_item",
                "timestamp": format!("2026-07-15T01:{minute}:01Z"),
                "payload": {"type": "message", "role": "user", "id": format!("turn-{session_id}"),
                    "content": [{"type": "input_text", "text": "inspect"}]}
            }),
            json!({
                "type": "response_item",
                "timestamp": format!("2026-07-15T01:{minute}:02Z"),
                "payload": {"type": "message", "role": "assistant", "id": format!("reply-{session_id}"),
                    "content": [{"type": "output_text", "text": "done"}]}
            }),
            json!({
                "type": "session_end",
                "timestamp": format!("2026-07-15T01:{minute}:03Z"),
                "payload": {"type": "session_end", "cwd": cwd}
            }),
        ];
        std::fs::write(
            day.join(format!(
                "rollout-2026-07-15T01-{minute}-00-{session_id}.jsonl"
            )),
            format!(
                "{}\n",
                lines
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .expect("write Codex rollout");
    }

    let output = fixture
        .command()
        .env("CODEX_HOME", &codex_home)
        .args([
            "agent", "import", "--all", "--agent", "codex", "--yes", "--json",
        ])
        .output()
        .expect("run mixed Codex batch");
    assert!(output.status.success(), "{}", describe(&output));
    let payload: Value = serde_json::from_slice(&output.stdout).expect("batch JSON");
    assert_eq!(payload["data"]["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["data"]["results"][0]["status"], "imported");
    assert_eq!(payload["data"]["skipped"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["data"]["skipped"][0]["status"], "skipped");
    assert_eq!(
        payload["data"]["skipped"][0]["reason_code"],
        "LBR-AGENT-015"
    );
    assert_eq!(
        payload["data"]["failures"].as_array().map(Vec::len),
        Some(0)
    );
}

#[test]
fn agent_import_cumulative_raw_input_cap_returns_sanitized_partial_result() {
    let fixture = ImportRepo::init();
    let first = fixture.write_discoverable_transcript("cap001", &fixture.repo);
    let second = fixture.write_discoverable_transcript("cap002", &fixture.repo);
    fixture.write_discoverable_transcript("cap003", &fixture.repo);
    let cap = std::fs::metadata(&first).expect("first metadata").len()
        + std::fs::metadata(&second).expect("second metadata").len()
        - 1;
    let output = fixture.run_with_env(
        &[
            "agent",
            "import",
            "--all",
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ],
        "LIBRA_TEST_IMPORT_BATCH_CAP_BYTES",
        &cap.to_string(),
    );
    assert!(!output.status.success(), "cap unexpectedly passed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-AGENT-018"), "{}", describe(&output));
    assert!(stderr.contains("\"succeeded\": 1"), "{}", describe(&output));
    assert!(!stderr.contains(fixture.home.to_string_lossy().as_ref()));
}

#[tokio::test]
async fn agent_import_attributes_index_drain_timeout_to_the_completed_candidate() {
    let fixture = ImportRepo::init();
    fixture.write_discoverable_transcript("a-index-first", &fixture.repo);
    fixture.write_discoverable_transcript("b-index-second", &fixture.repo);
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "5000")
        .env("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "10000")
        .args([
            "agent",
            "import",
            "--all",
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run delayed object-index batch");
    assert!(!output.status.success(), "{}", describe(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"succeeded\": 0"), "{}", describe(&output));
    assert!(stderr.contains("\"partial\": 1"), "{}", describe(&output));
    assert!(
        stderr.contains("\"session_id\": \"claude__a-index-first\"")
            && stderr.contains("\"status\": \"partial\""),
        "the candidate that enqueued the pending writes must own the partial result: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_checkpoint cp
                 JOIN agent_session s ON s.session_id = cp.session_id
                 WHERE s.provider_session_id = 'a-index-first'"
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_checkpoint cp
                 JOIN agent_session s ON s.session_id = cp.session_id
                 WHERE s.provider_session_id = 'b-index-second'"
            )
            .await,
        0,
        "the not-yet-started next candidate must not receive the first candidate's progress"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__a-index-first'"
            )
            .await,
        1,
        "a drain timeout must leave a durable replay-repair marker"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity
                 WHERE provider_session_id = 'a-index-first' AND state = 'partial'"
            )
            .await,
        1
    );

    let replay = fixture.run(&[
        "agent",
        "import",
        "--session",
        "a-index-first",
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(replay.status.success(), "{}", describe(&replay));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__a-index-first'"
            )
            .await,
        0,
        "successful replay must repair and retire the marker"
    );
    let doctor = fixture.run(&["agent", "doctor", "--json"]);
    assert!(doctor.status.success(), "{}", describe(&doctor));
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("missing_object_index"),
        "replay must repair the full checkpoint object set: {}",
        describe(&doctor)
    );
}

#[tokio::test]
async fn agent_import_index_update_error_is_partial_and_replay_repairs() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("index-error", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let first = fixture.run_with_env(
        &[
            "agent",
            "import",
            "--path",
            transcript_arg.as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ],
        "LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL",
        "1",
    );
    assert!(!first.status.success(), "{}", describe(&first));
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("LBR-AGENT-018"), "{}", describe(&first));
    assert!(stderr.contains("\"partial\": 1"), "{}", describe(&first));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-error'"
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity
                 WHERE provider_session_id = 'index-error' AND state = 'partial'"
            )
            .await,
        1
    );
    assert!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_checkpoint cp
                 WHERE cp.session_id = 'claude__index-error'
                   AND (
                     NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.traces_commit)
                     OR NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.tree_oid)
                     OR NOT EXISTS (
                         SELECT 1 FROM object_index oi WHERE oi.o_id = cp.metadata_blob_oid
                     )
                   )"
            )
            .await
            > 0,
        "the injected terminal queue failure must create a real repair target"
    );

    let replay = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(replay.status.success(), "{}", describe(&replay));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-error'"
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity
                 WHERE provider_session_id = 'index-error' AND state = 'committed'"
            )
            .await,
        1
    );
    let doctor = fixture.run(&["agent", "doctor", "--json"]);
    assert!(doctor.status.success(), "{}", describe(&doctor));
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("missing_object_index"),
        "foreground replay repair must restore every E4 index row: {}",
        describe(&doctor)
    );
}

#[tokio::test]
async fn import_index_tombstone_lookup_failure_is_partial_and_replay_repairs() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("index-tombstone-lookup", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run_with_env(&args, "LIBRA_TEST_IMPORT_INDEX_TOMBSTONE_LOOKUP_FAIL", "1");
    assert!(!first.status.success(), "{}", describe(&first));
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("LBR-AGENT-018"),
        "a failed tombstone recheck must fail closed: {}",
        describe(&first)
    );
    let marker = fixture
        .text_rows(
            "SELECT value FROM metadata_kv
             WHERE scope = 'agent_import_index_repair'
               AND target = 'claude__index-tombstone-lookup'",
            "value",
        )
        .await;
    assert_eq!(marker.len(), 1);
    let marker: Value = serde_json::from_str(&marker[0]).expect("decode pending barrier marker");
    assert_eq!(marker["state"], "repair_pending");
    assert_eq!(marker["lease_expires_at"], 0);

    let replay = fixture.run(&args);
    assert!(replay.status.success(), "{}", describe(&replay));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-tombstone-lookup'"
            )
            .await,
        0,
        "replay must repair and retire the lookup-failure marker"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_index_repair_serializes_with_session_erasure() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("index-repair-erase", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run_with_env(&args, "LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
    assert!(!first.status.success(), "{}", describe(&first));
    let checkpoint_oids = fixture
        .text_rows(
            "SELECT traces_commit AS oid FROM agent_checkpoint
             WHERE session_id = 'claude__index-repair-erase'
             UNION SELECT tree_oid AS oid FROM agent_checkpoint
             WHERE session_id = 'claude__index-repair-erase'
             UNION SELECT metadata_blob_oid AS oid FROM agent_checkpoint
             WHERE session_id = 'claude__index-repair-erase'",
            "oid",
        )
        .await;
    assert_eq!(checkpoint_oids.len(), 3);

    let repair_ready = fixture._tmp.path().join("repair-lock-ready");
    let repair_resume = fixture._tmp.path().join("repair-lock-resume");
    let barrier_ready = fixture._tmp.path().join("post-repair-barrier-ready");
    let barrier_resume = fixture._tmp.path().join("post-repair-barrier-resume");
    let replay = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_INDEX_REPAIR_READY_FILE", &repair_ready)
        .env(
            "LIBRA_TEST_IMPORT_INDEX_REPAIR_CONTINUE_FILE",
            &repair_resume,
        )
        .env("LIBRA_TEST_IMPORT_INDEX_BARRIER_READY_FILE", &barrier_ready)
        .env(
            "LIBRA_TEST_IMPORT_INDEX_BARRIER_CONTINUE_FILE",
            &barrier_resume,
        )
        .env("LIBRA_TEST_IMPORT_INDEX_TOMBSTONE_LOOKUP_FAIL", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("spawn replay repair");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !repair_ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        repair_ready.exists(),
        "repair helper did not acquire SQLite writer lock"
    );

    let repo = fixture.repo.clone();
    let (erase_tx, erase_rx) = std::sync::mpsc::channel();
    let erase_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build erasure runtime");
        let result = runtime.block_on(async move {
            let libra_dir = repo.join(".libra");
            let db_url = format!("sqlite://{}", libra_dir.join("libra.db").display());
            let conn = Database::connect(db_url)
                .await
                .map_err(|error| format!("open erasure database: {error}"))?;
            let history = HistoryManager::new_with_ref(
                Arc::new(ClientStorage::init(libra_dir.join("objects"))),
                libra_dir,
                Arc::new(conn),
                TRACES_BRANCH,
            );
            history
                .erase_session_local("claude__index-repair-erase")
                .await
                .map(|outcome| outcome.session_deleted)
                .map_err(|error| format!("erase repaired import session: {error:#}"))
        });
        erase_tx.send(result).expect("send erasure outcome");
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        matches!(
            erase_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "erasure must wait while the repair transaction owns the SQLite writer slot"
    );

    std::fs::write(&repair_resume, b"continue").expect("resume repair helper");
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier_ready.exists() && Instant::now() < barrier_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        barrier_ready.exists(),
        "replay did not return from fenced repair"
    );
    let erased = erase_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("erasure did not finish after repair transaction committed")
        .expect("session erasure failed");
    assert!(erased);
    erase_thread.join().expect("join erasure thread");

    std::fs::write(&barrier_resume, b"continue").expect("resume tombstoned replay");
    let replay = replay
        .wait_with_output()
        .expect("wait for tombstoned replay");
    assert!(
        !replay.status.success(),
        "erased replay succeeded: {}",
        describe(&replay)
    );
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("LBR-AGENT-019"),
        "tombstone-winning erasure must retain its actionable error code: {}",
        describe(&replay)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-repair-erase'"
            )
            .await,
        0,
        "erasure must retire the owned repair marker"
    );
    let oid_list = checkpoint_oids
        .iter()
        .map(|oid| format!("'{oid}'"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        fixture
            .scalar(&format!(
                "SELECT COUNT(*) AS n FROM object_index WHERE o_id IN ({oid_list})"
            ))
            .await,
        0,
        "fenced repair must not reinsert cloud-eligible index rows after erasure"
    );
}

#[tokio::test]
async fn import_index_repair_timeout_rolls_back_and_preserves_pending_barrier() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("index-repair-timeout", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let args = [
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run_with_env(&args, "LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
    assert!(!first.status.success(), "{}", describe(&first));
    let missing_before = fixture
        .scalar(
            "SELECT COUNT(*) AS n FROM agent_checkpoint cp
             WHERE cp.session_id = 'claude__index-repair-timeout'
               AND (
                 NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.traces_commit)
                 OR NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.tree_oid)
                 OR NOT EXISTS (
                     SELECT 1 FROM object_index oi WHERE oi.o_id = cp.metadata_blob_oid
                 )
               )",
        )
        .await;
    assert!(missing_before > 0);

    let ready = fixture._tmp.path().join("repair-timeout-ready");
    let never_resume = fixture._tmp.path().join("repair-timeout-never-resume");
    let started = Instant::now();
    let replay = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "3000")
        // Keep the durable command preflight from repairing the deliberately
        // missing rows before this test reaches the import helper transaction.
        .env("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1")
        .env("LIBRA_TEST_IMPORT_INDEX_REPAIR_READY_FILE", &ready)
        .env(
            "LIBRA_TEST_IMPORT_INDEX_REPAIR_CONTINUE_FILE",
            &never_resume,
        )
        .args(args)
        .output()
        .expect("run bounded repair timeout");
    assert!(!replay.status.success(), "repair timeout succeeded");
    assert!(
        ready.exists(),
        "repair helper never entered its writer transaction"
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "killable repair helper exceeded the aggregate import deadline"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_checkpoint cp
                 WHERE cp.session_id = 'claude__index-repair-timeout'
                   AND (
                     NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.traces_commit)
                     OR NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.tree_oid)
                     OR NOT EXISTS (
                         SELECT 1 FROM object_index oi WHERE oi.o_id = cp.metadata_blob_oid
                     )
                   )"
            )
            .await,
        missing_before,
        "killing the helper must roll back every object-index mutation"
    );
    let marker_values = fixture
        .text_rows(
            "SELECT value FROM metadata_kv
             WHERE scope = 'agent_import_index_repair'
               AND target = 'claude__index-repair-timeout'",
            "value",
        )
        .await;
    assert_eq!(marker_values.len(), 1);
    let marker: Value = serde_json::from_str(&marker_values[0]).expect("parse pending barrier");
    assert_eq!(marker["state"], "repair_pending");
    assert_eq!(marker["lease_expires_at"], 0);
}

#[tokio::test]
async fn agent_import_charges_subagent_bytes_to_cumulative_raw_input_cap() {
    let fixture = ImportRepo::init();
    let session_id = "abcdef00-0000-0000-0000-000000000010";
    let parent = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let child_dir = parent
        .parent()
        .expect("Claude project directory")
        .join(session_id)
        .join("subagents");
    std::fs::create_dir_all(&child_dir).expect("subagent directory");
    let child = child_dir.join("child.jsonl");
    let child_body = format!(
        "{}\n",
        json!({
            "type": "assistant",
            "uuid": "child-assistant",
            "message": {
                "role": "assistant",
                "content": "child result",
                "usage": {"input_tokens": 2, "output_tokens": 1}
            }
        })
    );
    std::fs::write(&child, child_body).expect("child transcript");
    let cap = std::fs::metadata(&parent).expect("parent metadata").len()
        + std::fs::metadata(&child).expect("child metadata").len()
        - 1;
    let output = fixture.run_with_env(
        &[
            "agent",
            "import",
            "--path",
            path_arg(&parent).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ],
        "LIBRA_TEST_IMPORT_BATCH_CAP_BYTES",
        &cap.to_string(),
    );
    assert!(
        !output.status.success(),
        "subagent bytes bypassed batch cap"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0,
        "budget failure must happen before parent or child persistence"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_subagent_content_claim")
            .await,
        0
    );
}

#[tokio::test]
async fn agent_import_reports_child_only_replay_as_imported() {
    let fixture = ImportRepo::init();
    let session_id = "abcdef00-0000-0000-0000-000000000012";
    let parent = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let parent_arg = path_arg(&parent);
    let args = [
        "agent",
        "import",
        "--path",
        parent_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ];
    let first = fixture.run(&args);
    assert!(first.status.success(), "first import: {}", describe(&first));

    let child_dir = parent
        .parent()
        .expect("Claude project directory")
        .join(session_id)
        .join("subagents");
    std::fs::create_dir_all(&child_dir).expect("subagent directory");
    std::fs::write(
        child_dir.join("child.jsonl"),
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "uuid": "child-assistant",
                "message": {"role": "assistant", "content": "late child"}
            })
        ),
    )
    .expect("late child transcript");

    let replay = fixture.run(&args);
    assert!(
        replay.status.success(),
        "child-only replay: {}",
        describe(&replay)
    );
    let payload: Value = serde_json::from_slice(&replay.stdout).expect("replay JSON");
    assert_eq!(payload["data"]["results"][0]["status"], "imported");
    assert_eq!(payload["data"]["results"][0]["checkpoints_written"], 0);
    assert_eq!(
        payload["data"]["results"][0]["subagent_checkpoints_written"],
        1
    );
}

#[tokio::test]
async fn agent_import_marks_malformed_subagent_content_partial() {
    let fixture = ImportRepo::init();
    let session_id = "abcdef00-0000-0000-0000-000000000011";
    let parent = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let child_dir = parent
        .parent()
        .expect("Claude project directory")
        .join(session_id)
        .join("subagents");
    std::fs::create_dir_all(&child_dir).expect("subagent directory");
    std::fs::write(
        child_dir.join("child.jsonl"),
        format!(
            "not-json\n{}\n",
            json!({
                "type": "assistant",
                "uuid": "child-assistant",
                "message": {"role": "assistant", "content": "partial child"}
            })
        ),
    )
    .expect("malformed child transcript");

    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&parent).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        !output.status.success(),
        "malformed child content must make the import partial: {}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity
                 WHERE state = 'partial' AND last_error_code = 'LBR-AGENT-018'",
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_subagent_content_revision WHERE partial = 1",)
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_checkpoint cp
                 WHERE cp.scope = 'subagent'
                   AND (
                     NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.traces_commit)
                     OR NOT EXISTS (SELECT 1 FROM object_index oi WHERE oi.o_id = cp.tree_oid)
                     OR NOT EXISTS (
                         SELECT 1 FROM object_index oi WHERE oi.o_id = cp.metadata_blob_oid
                     )
                   )",
            )
            .await,
        0,
        "partial import must drain every durable checkpoint object-index write before error exit"
    );
}

#[tokio::test]
async fn agent_import_marks_empty_subagent_content_partial() {
    let fixture = ImportRepo::init();
    let session_id = "abcdef00-0000-0000-0000-000000000012";
    let parent = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let child_dir = parent
        .parent()
        .expect("Claude project directory")
        .join(session_id)
        .join("subagents");
    std::fs::create_dir_all(&child_dir).expect("subagent directory");
    std::fs::write(child_dir.join("empty.jsonl"), b"").expect("empty child transcript");

    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&parent).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        !output.status.success(),
        "empty child evidence must make the import partial: {}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_subagent_content_revision WHERE partial = 1")
            .await,
        1
    );
}

#[tokio::test]
async fn agent_import_late_child_validation_preserves_partial_parent() {
    let fixture = ImportRepo::init();
    let session_id = "abcdef00-0000-0000-0000-000000000013";
    let parent = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let child_dir = parent
        .parent()
        .expect("Claude project directory")
        .join(session_id)
        .join("subagents");
    std::fs::create_dir_all(&child_dir).expect("subagent directory");
    std::fs::write(
        child_dir.join("child.jsonl"),
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "uuid": "late-child-assistant",
                "message": {"role": "assistant", "content": "late child"}
            })
        ),
    )
    .expect("child transcript");

    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "10000")
        .env("LIBRA_TEST_SUBAGENT_PARENT_VALIDATION_DELAY_MS", "6000")
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&parent).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run late child validation import");
    assert!(!output.status.success(), "{}", describe(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-AGENT-018"), "{}", describe(&output));
    assert!(
        stderr.contains("\"status\": \"partial\"") && stderr.contains("\"checkpoints_written\": 1"),
        "late child validation must retain exact parent progress: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'committed'")
            .await,
        1,
        "the independently valid parent checkpoint must be durable"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'subagent'")
            .await,
        0,
        "unvalidated child content must not be persisted"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_identity WHERE state = 'partial'")
            .await,
        1
    );
}

#[tokio::test]
async fn agent_import_failed_candidate_bytes_reduce_remaining_batch_budget() {
    let fixture = ImportRepo::init();
    let malformed = fixture.write_discoverable_transcript("bad001", &fixture.repo);
    std::fs::write(&malformed, vec![b'{'; 900]).expect("write malformed charged candidate");
    fixture.write_discoverable_transcript("good002", &fixture.repo);
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_BATCH_CAP_BYTES", "1000")
        .args([
            "agent",
            "import",
            "--all",
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run failed-input budget probe");
    assert!(!output.status.success(), "failed bytes were not budgeted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"succeeded\": 0"), "{}", describe(&output));
    assert!(stderr.contains("\"failed\": 2"), "{}", describe(&output));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "the valid second candidate was read as though the failed first read were free"
    );
}

#[tokio::test]
async fn agent_import_charges_bytes_read_from_growing_held_descriptor() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("growing123", &fixture.repo, true);
    let initial_len = std::fs::metadata(&transcript)
        .expect("initial transcript metadata")
        .len();
    let ready = fixture._tmp.path().join("source-ready");
    let resume = fixture._tmp.path().join("source-resume");
    let mut command = fixture.command();
    let child = command
        .env(
            "LIBRA_TEST_IMPORT_BATCH_CAP_BYTES",
            (initial_len + 8).to_string(),
        )
        .env("LIBRA_TEST_IMPORT_SOURCE_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_SOURCE_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .spawn()
        .expect("spawn source-pinned importer");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "importer did not open the held descriptor");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("open growing transcript")
        .write_all(b"0123456789abcdef0123456789abcdef")
        .expect("grow transcript after authorization");
    std::fs::write(&resume, b"continue").expect("resume source read");
    let output = child.wait_with_output().expect("wait for growing import");
    assert!(
        !output.status.success(),
        "growing source bypassed batch cap"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "over-budget held bytes must be rejected before persistence"
    );
}

#[tokio::test]
async fn agent_import_deadline_abandons_bound_leases_and_marker() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("deadline123", &fixture.repo, true);
    let ready = fixture._tmp.path().join("deadline-ready");
    let never_resume = fixture._tmp.path().join("deadline-never-resume");
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "500")
        .env("LIBRA_TEST_IMPORT_AFTER_BIND_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_AFTER_BIND_CONTINUE_FILE", &never_resume)
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run deadline-bound import");
    assert!(
        !output.status.success(),
        "deadline import unexpectedly passed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_import_identity \
                 WHERE state = 'failed' AND owner IS NULL AND lease_expires_at IS NULL"
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
            .await,
        0,
        "zero-progress provisional session cleanup cascades abandoned claims"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv \
                 WHERE scope = 'agent_traces_inflight'"
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "a terminal zero-progress import must remove its provisional session"
    );
}

#[tokio::test]
async fn agent_import_deadline_kills_blocked_authorized_reader_process() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("slowread123", &fixture.repo, true);
    let reader_pid_file = fixture._tmp.path().join("authorized-reader.pid");
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "150")
        .env("LIBRA_TEST_AUTHORIZED_READ_HELPER_DELAY_MS", "5000")
        .env(
            "LIBRA_TEST_AUTHORIZED_READ_HELPER_PID_FILE",
            &reader_pid_file,
        )
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run blocked authorized-reader deadline probe");
    assert!(!output.status.success(), "slow reader bypassed deadline");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "killable reader process outlived the absolute deadline: {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    let reader_pid = std::fs::read_to_string(&reader_pid_file)
        .expect("nested authorized reader published its pid")
        .parse::<u32>()
        .expect("authorized reader pid is numeric");
    let reap_deadline = Instant::now() + Duration::from_secs(2);
    let reader_proc = format!("/proc/{reader_pid}");
    while std::path::Path::new(&reader_proc).exists() && Instant::now() < reap_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !std::path::Path::new(&reader_proc).exists(),
        "timed-out preparation left nested authorized reader pid {reader_pid} alive"
    );
}

#[tokio::test]
async fn agent_import_deadline_kills_helper_blocked_at_secure_open_stage() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("secureopen123", &fixture.repo, true);
    let ready = fixture._tmp.path().join("secure-open-ready");
    let never_continue = fixture._tmp.path().join("secure-open-never-continue");
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "200")
        .env("LIBRA_TEST_IMPORT_SECURE_OPEN_READY_FILE", &ready)
        .env(
            "LIBRA_TEST_IMPORT_SECURE_OPEN_CONTINUE_FILE",
            &never_continue,
        )
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run secure-open deadline probe");
    assert!(
        !output.status.success(),
        "blocked secure open bypassed deadline"
    );
    assert!(ready.exists(), "probe never entered the secure-open stage");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "secure-open helper outlived the absolute deadline: {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
}

#[tokio::test]
async fn agent_import_deadline_kills_helper_blocked_at_transcript_cwd_resolution() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("cwdblock123", &fixture.repo, true);
    let ready = fixture._tmp.path().join("transcript-cwd-ready");
    let never_continue = fixture._tmp.path().join("transcript-cwd-never-continue");
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "500")
        .env("LIBRA_TEST_IMPORT_TRANSCRIPT_CWD_READY_FILE", &ready)
        .env(
            "LIBRA_TEST_IMPORT_TRANSCRIPT_CWD_CONTINUE_FILE",
            &never_continue,
        )
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run transcript-cwd deadline probe");
    assert!(
        !output.status.success(),
        "blocked cwd lookup bypassed deadline"
    );
    assert!(
        ready.exists(),
        "probe never entered transcript cwd resolution"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "transcript-cwd helper outlived the deadline: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn agent_import_deadline_bounds_preparation_response_pipe_drain() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("responsepipe123", &fixture.repo, true);
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "500")
        .env(
            "LIBRA_TEST_IMPORT_PREPARATION_RESPONSE_READ_DELAY_MS",
            "2000",
        )
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run delayed preparation-response deadline probe");
    assert!(
        !output.status.success(),
        "delayed preparation response bypassed deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "response pipe drain outlived the absolute deadline: {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "preparation response timeout persisted a session"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0,
        "preparation response timeout persisted a checkpoint"
    );
}

#[tokio::test]
async fn agent_import_deadline_kills_helper_blocked_at_existing_session_cwd_revalidation() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("existingcwd123", &fixture.repo, true);
    let first = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&transcript).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(first.status.success(), "seed import: {}", describe(&first));
    let checkpoints_before = fixture
        .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
        .await;
    let ready = fixture._tmp.path().join("existing-cwd-ready");
    let never_continue = fixture._tmp.path().join("existing-cwd-never-continue");
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "750")
        .env("LIBRA_TEST_IMPORT_EXISTING_CWD_READY_FILE", &ready)
        .env(
            "LIBRA_TEST_IMPORT_EXISTING_CWD_CONTINUE_FILE",
            &never_continue,
        )
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run existing-cwd deadline probe");
    assert!(
        !output.status.success(),
        "blocked ownership revalidation bypassed deadline"
    );
    assert!(
        ready.exists(),
        "probe never entered existing cwd revalidation"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "existing-session cwd helper outlived deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        checkpoints_before,
        "timed-out ownership validation mutated durable import state"
    );
}

#[tokio::test]
async fn agent_import_deadline_cancels_slow_append_before_objects() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("slowappend123", &fixture.repo, true);
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "150")
        .env("LIBRA_TEST_CHECKPOINT_APPEND_DELAY_MS", "1500")
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run slow append deadline probe");
    assert!(!output.status.success(), "slow append bypassed deadline");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "append delay was not cancelled at the absolute deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );
}

#[tokio::test]
async fn agent_import_deadline_kills_blocked_checkpoint_object_write() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("blockedobject123", &fixture.repo, true);
    let ready = fixture._tmp.path().join("checkpoint-object-write-ready");
    let started = Instant::now();
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "500")
        .env("LIBRA_TEST_CHECKPOINT_OBJECT_WRITE_READY_FILE", &ready)
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run blocked object-write deadline probe");
    assert!(
        ready.exists(),
        "import never reached checkpoint object write"
    );
    assert!(
        !output.status.success(),
        "blocked object write bypassed deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "blocked object helper held the foreground past its deadline: {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "{}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv \
                 WHERE scope = 'agent_traces_inflight'"
            )
            .await,
        0,
        "timed-out object preclaim/marker was not abandoned"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
            .await,
        0,
        "timed-out object write retained provisional coverage claims"
    );
}

#[tokio::test]
async fn agent_import_reports_success_when_deadline_expires_after_atomic_commit() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("postcommitdeadline", &fixture.repo, true);
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "2000")
        .env("LIBRA_TEST_CHECKPOINT_POST_COMMIT_DELAY_MS", "2500")
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run post-commit deadline probe");
    assert!(
        output.status.success(),
        "a fully committed import was reported partial: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_import_identity WHERE state = 'committed'")
            .await,
        1
    );
}

#[tokio::test]
async fn agent_import_deadline_abandons_prior_reservation_batches() {
    let fixture = ImportRepo::init();
    let path = fixture.transcript_path("manyturns123");
    std::fs::create_dir_all(path.parent().expect("transcript parent"))
        .expect("create transcript dir");
    let mut lines = Vec::new();
    for ordinal in 0..130 {
        lines.push(json!({
            "type": "user", "uuid": format!("turn-{ordinal}"),
            "sessionId": "manyturns123", "cwd": fixture.repo,
            "message": {"role": "user", "content": format!("question {ordinal}")}
        }));
        lines.push(json!({
            "type": "assistant", "uuid": format!("answer-{ordinal}"),
            "sessionId": "manyturns123", "cwd": fixture.repo,
            "message": {"role": "assistant", "content": [{"type":"text", "text":"ok"}]}
        }));
    }
    lines.push(json!({
        "type": "session_end", "sessionId": "manyturns123", "cwd": fixture.repo
    }));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write many-turn transcript");
    let output = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_DEADLINE_MS", "150")
        .env("LIBRA_TEST_IMPORT_RESERVATION_BATCH_DELAY_MS", "100")
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&path).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .output()
        .expect("run batched reservation deadline probe");
    assert!(
        !output.status.success(),
        "reservation batching bypassed deadline"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
            .await,
        0,
        "reservations from committed earlier batches were not abandoned/cascaded"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );
}

#[tokio::test]
async fn agent_import_marker_failure_is_fail_closed_and_preserves_existing_session() {
    let fixture = ImportRepo::init();
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TRIGGER reject_import_marker
         BEFORE INSERT ON metadata_kv
         WHEN NEW.scope = 'agent_traces_inflight'
         BEGIN
             SELECT RAISE(ABORT, 'test marker write failure');
         END"
        .to_string(),
    ))
    .await
    .expect("install marker rejection trigger");

    let fresh = fixture.write_transcript("markerfail-new", &fixture.repo, true);
    let objects_before = loose_object_file_count(&fixture.repo.join(".libra/objects"));
    let fresh_output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&fresh).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!fresh_output.status.success(), "marker failure passed");
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0,
        "marker failure left a provisional session"
    );
    assert_eq!(
        loose_object_file_count(&fixture.repo.join(".libra/objects")),
        objects_before,
        "marker failure must happen before object construction"
    );

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            stopped_at, schema_version
         ) VALUES (?, 'claude_code', ?, 'active', ?, ?, '{}', 11, 22, NULL, 1)",
        [
            "claude__markerfail-existing".into(),
            "markerfail-existing".into(),
            fixture.repo.to_string_lossy().into_owned().into(),
            serde_json::json!({"sentinel":"keep"}).to_string().into(),
        ],
    ))
    .await
    .expect("seed existing live session");
    let existing = fixture.write_transcript("markerfail-existing", &fixture.repo, true);
    let existing_output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&existing).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!existing_output.status.success(), "marker failure passed");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT state, working_dir, metadata_json, last_event_at, stopped_at
             FROM agent_session WHERE session_id = 'claude__markerfail-existing'"
                .to_string(),
        ))
        .await
        .expect("query preserved session")
        .expect("existing session remains");
    assert_eq!(
        row.try_get_by::<String, _>("state").expect("state"),
        "active"
    );
    assert_eq!(row.try_get_by::<i64, _>("last_event_at").expect("time"), 22);
    assert_eq!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .expect("stopped"),
        None
    );
    assert_eq!(
        row.try_get_by::<String, _>("metadata_json")
            .expect("metadata"),
        serde_json::json!({"sentinel":"keep"}).to_string(),
        "failed import mutated existing session ownership metadata"
    );
}

#[tokio::test]
async fn erasure_winning_before_marker_prevents_all_import_objects() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_transcript("erase-before-marker", &fixture.repo, true);
    let ready = fixture._tmp.path().join("before-marker-ready");
    let resume = fixture._tmp.path().join("before-marker-resume");
    let objects_before = loose_object_file_count(&fixture.repo.join(".libra/objects"));
    let child = fixture
        .command()
        .env("LIBRA_TEST_IMPORT_BEFORE_BIND_READY_FILE", &ready)
        .env("LIBRA_TEST_IMPORT_BEFORE_BIND_CONTINUE_FILE", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "agent",
            "import",
            "--path",
            path_arg(&transcript).as_str(),
            "--agent",
            "claude-code",
            "--yes",
            "--json",
        ])
        .spawn()
        .expect("spawn pre-marker importer");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "importer did not reach pre-marker pause");
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0
    );

    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(conn),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local("claude__erase-before-marker")
        .await
        .expect("erasure should win before marker registration");
    assert!(erased.session_deleted);
    std::fs::write(&resume, b"resume").expect("resume stale importer");
    let output = child.wait_with_output().expect("wait stale importer");
    assert!(!output.status.success(), "stale importer survived erasure");
    assert_eq!(
        loose_object_file_count(&fixture.repo.join(".libra/objects")),
        objects_before,
        "tombstone winner must reject bind before object construction"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_session")
            .await,
        0
    );
}

#[cfg(unix)]
#[test]
fn agent_import_rejects_symlinked_source_fail_closed() {
    use std::os::unix::fs::symlink;

    let fixture = ImportRepo::init();
    let real = fixture.write_transcript("real123", &fixture.repo, true);
    let link = fixture.transcript_path("link123");
    symlink(&real, &link).expect("create transcript symlink");
    let output = fixture.run(&[
        "agent",
        "import",
        "--path",
        path_arg(&link).as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "{}",
        describe(&output)
    );
}

#[test]
fn agent_list_schema_v1_stays_frozen_and_v2_is_explicit() {
    let fixture = ImportRepo::init();
    let v1 = fixture.run(&["agent", "list", "--json"]);
    assert!(v1.status.success(), "v1 list: {}", describe(&v1));
    let v1: Value = serde_json::from_slice(&v1.stdout).expect("v1 JSON");
    assert_eq!(v1["data"]["schema_version"], 1);
    let v1_row = v1["data"]["agents"][0].as_object().expect("v1 agent row");
    let v1_keys = v1_row.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        v1_keys,
        vec![
            "agent_kind",
            "capabilities",
            "config_paths",
            "db_value",
            "external_binary",
            "hook_installable",
            "installed",
            "launchable_investigate",
            "launchable_review",
            "protected_dirs",
            "provider_name",
            "registered",
            "slug",
            "stability",
            "support_wave",
            "supported",
            "transcript_readable",
        ],
        "v1 row key set/order is the frozen compatibility fixture"
    );

    let v2 = fixture.run(&["agent", "list", "--schema-version", "2", "--json"]);
    assert!(v2.status.success(), "v2 list: {}", describe(&v2));
    let v2: Value = serde_json::from_slice(&v2.stdout).expect("v2 JSON");
    assert_eq!(v2["data"]["schema_version"], 2);
    for method in v2["data"]["agents"][0]["methods"]
        .as_array()
        .expect("v2 methods")
    {
        assert_eq!(
            method
                .as_object()
                .expect("method object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["available", "name", "supported", "unavailable_reason"]
        );
    }
    let opencode = v2["data"]["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|row| row["slug"] == "opencode")
        .expect("opencode row");
    let discover = opencode["methods"]
        .as_array()
        .expect("methods")
        .iter()
        .find(|method| method["name"] == "transcript_discoverable")
        .expect("discovery method");
    assert_eq!(discover["supported"], false);
    assert_eq!(discover["available"], false);

    let unsupported = fixture.run(&["agent", "list", "--schema-version", "3", "--json"]);
    assert!(!unsupported.status.success());
    assert_eq!(unsupported.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&unsupported.stderr).contains("LBR-AGENT-017"),
        "{}",
        describe(&unsupported)
    );
}
