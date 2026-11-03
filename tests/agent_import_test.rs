//! plan-20260713 M4 historical transcript import contract.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
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
            observed_agents::{
                AgentKind, normalize_claude_transcript, redact_turns, safe_turn_projection,
            },
        },
        branch::TRACES_BRANCH,
    },
    utils::{client_storage::ClientStorage, object::read_git_object},
};
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

    fn session_end_hook(&self, session_id: &str, transcript_path: &Path) -> Output {
        self.session_end_hook_from_cwd(&self.repo, session_id, transcript_path)
    }

    fn session_end_hook_from_cwd(
        &self,
        cwd: &Path,
        session_id: &str,
        transcript_path: &Path,
    ) -> Output {
        let envelope = json!({
            "hook_event_name": "SessionEnd",
            "session_id": session_id,
            "cwd": cwd.to_string_lossy(),
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

    fn discoverable_transcript_path_for_cwd(&self, session_id: &str, cwd: &Path) -> PathBuf {
        let slug = cwd
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

    fn discoverable_transcript_path(&self, session_id: &str) -> PathBuf {
        self.discoverable_transcript_path_for_cwd(session_id, &self.repo)
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
        self.write_discoverable_transcript_for_cwd(session_id, cwd)
    }

    fn write_discoverable_transcript_for_cwd(&self, session_id: &str, cwd: &Path) -> PathBuf {
        let source = self.write_transcript(session_id, cwd, true);
        let destination = self.discoverable_transcript_path_for_cwd(session_id, cwd);
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

/// Captured from the v0.30.46 descriptor helper at source commit
/// 077e66f75a5bf4bb793b95f904c82017fa1f0e86 (before OG-20). Reproduce
/// with that release's `libra --libra-internal-agent-import-preparation-descriptor-helper`,
/// `LIBRA_INTERNAL_IMPORT_PREPARATION_DESCRIPTOR_CONTROL` from
/// `empty_preparation_control`, and `empty_preparation_source` on stdin after
/// `libra init` in `ImportRepo`. The pre-change binary returned status=ok,
/// raw_bytes=699 in the recorded local fixture and the pinned content digest
/// below. Only values derived from the temporary repo path and source bytes
/// are substituted; field names, order, punctuation and fixed values are pinned.
const LEGACY_EMPTY_PREPARATION_PROJECTION: &str = r#"{"content_digest":"284e1514b809c031303fd684e91a92715b906cb8c6619e1993a5d3dde2eb0473","started_at":1784077200,"ended_at":1784077202,"session_state":"stopped","stopped_at":1784077202,"storage_commitment":@STORAGE_COMMITMENT@,"redaction_report":{"bytes_redacted":0,"bytes_scanned":@TOTAL_SCANNED@,"matches":[],"pipeline":"typed_allowlist","raw_persisted":false,"snapshot_redaction":true},"transcript_snapshot":{"completeness":"complete","partial_reason":null,"source":{"kind":"trusted_export","identity":"not_retained:v1","digest_sha256":"sha256:@SOURCE_SHA256@","byte_len":@SOURCE_LEN@},"transcript_redacted_bytes":@SOURCE_LEN@,"redaction_match_count":0,"redaction_bytes_scanned":@SOURCE_LEN@,"redaction_bytes_redacted":0},"turn_boundaries":{"turn-1":{"started_at":1784077200,"ended_at":1784077201}},"turns":[{"logical_turn_key":"turn-1","ordinal":0,"completeness":"complete","started_at":1784077200,"ended_at":1784077201,"records":[{"kind":"user","text":"inspect the repo"},{"kind":"assistant","text":"done"}]}]}"#;

const EMPTY_PREPARATION_SESSION: &str = "og20-empty-wire";

fn empty_preparation_source(fixture: &ImportRepo) -> Vec<u8> {
    let lines = [
        json!({
            "type": "user",
            "uuid": "turn-1",
            "sessionId": EMPTY_PREPARATION_SESSION,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:00Z",
            "message": {"role": "user", "content": "inspect the repo"}
        }),
        json!({
            "type": "assistant",
            "uuid": "assistant-1",
            "sessionId": EMPTY_PREPARATION_SESSION,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:01Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
        }),
        json!({
            "type": "session_end",
            "sessionId": EMPTY_PREPARATION_SESSION,
            "cwd": fixture.repo,
            "timestamp": "2026-07-15T01:00:02Z"
        }),
    ];
    format!(
        "{}\n",
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    )
    .into_bytes()
}

fn empty_preparation_control(source_len: usize) -> String {
    // Freeze the pre-OG-20 domain and framing independently of production's
    // private commitment builder so a changed request cannot self-confirm.
    let mut commitment = Sha256::new();
    commitment.update(b"libra-agent-import-provider-commitment-v2\0");
    commitment.update((EMPTY_PREPARATION_SESSION.len() as u64).to_be_bytes());
    commitment.update(EMPTY_PREPARATION_SESSION.as_bytes());
    let provider_commitment: [u8; 32] = commitment.finalize().into();
    json!({
        "agent_kind": "claude-code",
        "source_kind": "file",
        "provider_commitment": provider_commitment,
        "read_cap": source_len + 1,
        "remaining_ms": 10_000,
    })
    .to_string()
}

fn run_real_empty_preparation_helper(fixture: &ImportRepo, source: &[u8]) -> Output {
    let mut child = fixture
        .command()
        .arg(libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG)
        .env(
            libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV,
            empty_preparation_control(source.len()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn real Libra descriptor helper");
    child
        .stdin
        .as_mut()
        .expect("helper stdin")
        .write_all(source)
        .expect("write held descriptor bytes");
    child
        .wait_with_output()
        .expect("wait for real descriptor helper")
}

fn legacy_empty_projection_bytes(fixture: &ImportRepo, source: &[u8]) -> Vec<u8> {
    let storage = fixture
        .repo
        .join(".libra")
        .canonicalize()
        .expect("canonical fixture storage root");
    let storage_text = storage.to_str().expect("UTF-8 fixture storage root");
    let mut commitment = Sha256::new();
    commitment.update(b"libra-agent-import-storage-commitment-v2\0");
    commitment.update((storage_text.len() as u64).to_be_bytes());
    commitment.update(storage_text.as_bytes());
    let storage_commitment: [u8; 32] = commitment.finalize().into();
    let storage_bytes = format!(
        "[{}]",
        storage_commitment
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let source_sha256 = hex::encode(Sha256::digest(source));
    // The pre-change typed pass scanned 26 ordinary semantic bytes in this
    // fixture in addition to the exact authorized source byte count.
    LEGACY_EMPTY_PREPARATION_PROJECTION
        .replace("@STORAGE_COMMITMENT@", &storage_bytes)
        .replace("@TOTAL_SCANNED@", &(source.len() + 26).to_string())
        .replace("@SOURCE_SHA256@", &source_sha256)
        .replace("@SOURCE_LEN@", &source.len().to_string())
        .into_bytes()
}

#[test]
fn reasoning_empty_preparation_wire_is_unchanged() {
    let fixture = ImportRepo::init();
    let source = empty_preparation_source(&fixture);
    let output = run_real_empty_preparation_helper(&fixture, &source);
    assert!(output.status.success(), "{}", describe(&output));
    let projection = legacy_empty_projection_bytes(&fixture, &source);
    let mut expected = b"{\"status\":\"ok\",\"projection\":".to_vec();
    expected.extend_from_slice(&projection);
    expected.extend_from_slice(format!(",\"raw_bytes\":{}}}", source.len()).as_bytes());
    assert_eq!(
        output.stdout, expected,
        "legacy helper response bytes drifted"
    );
}

#[test]
fn reasoning_empty_preparation_projection_is_unchanged() {
    let fixture = ImportRepo::init();
    let source = empty_preparation_source(&fixture);
    let output = run_real_empty_preparation_helper(&fixture, &source);
    assert!(output.status.success(), "{}", describe(&output));
    let prefix = b"{\"status\":\"ok\",\"projection\":";
    let suffix = format!(",\"raw_bytes\":{}}}", source.len());
    assert!(output.stdout.starts_with(prefix));
    assert!(output.stdout.ends_with(suffix.as_bytes()));
    let projection = &output.stdout[prefix.len()..output.stdout.len() - suffix.len()];
    assert_eq!(
        projection,
        legacy_empty_projection_bytes(&fixture, &source),
        "legacy source projection bytes drifted"
    );
}

#[test]
fn reasoning_private_helper_control_frame_is_bounded() {
    let fixture = ImportRepo::init();
    let source = empty_preparation_source(&fixture);
    let mut control = empty_preparation_control(source.len());
    control.extend(std::iter::repeat_n(' ', 4 * 1024 + 1 - control.len()));
    assert_eq!(control.len(), 4 * 1024 + 1);
    let mut child = fixture
        .command()
        .arg(libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG)
        .env(
            libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV,
            control,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn real Libra descriptor helper");
    // Hold stdin open and send no bytes: an oversized control must be rejected
    // before the helper touches its descriptor, with no response frame.
    let held_stdin = child.stdin.take().expect("held helper stdin");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll real descriptor helper") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop hung real descriptor helper");
            let _ = child.wait();
            panic!("oversized control reached the descriptor read");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(held_stdin);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("helper stdout")
        .read_to_end(&mut stdout)
        .expect("read helper stdout");
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "rejected control emitted a response");
}

/// Auxiliary forward-recovery evidence for the production-disabled native
/// path: a real pending checkpoint artifact survives both an unsupported
/// private control version and a subsequent ordinary import attempt.
/// A nonempty authenticated DTO cannot be injected into the production
/// parent while no provider native issuer is registered, so this is not the
/// full OG-20 VER18 candidate/transport acceptance gate.
#[cfg(unix)]
#[tokio::test]
async fn reasoning_private_helper_old_control_preserves_pending_artifact_evidence() {
    let fixture = ImportRepo::init();
    let session = "og20-retained-artifact";
    let transcript = fixture.write_discoverable_transcript(session, &fixture.repo);
    let run_hook = |event: &str, verb: &str, budget: bool| {
        let envelope = json!({
            "hook_event_name": event,
            "session_id": session,
            "cwd": fixture.repo,
            "transcript_path": transcript,
            "event_id": format!("og20-{verb}-v1"),
        })
        .to_string();
        let mut command = fixture.command();
        command.args(["agent", "hooks"]);
        if budget {
            command.args(["--capture-budget-ms", "60000"]);
        }
        let mut child = command
            .args(["claude-code", verb])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn real hook handler");
        child
            .stdin
            .as_mut()
            .expect("hook stdin")
            .write_all(envelope.as_bytes())
            .expect("write hook envelope");
        child.wait_with_output().expect("wait for real hook")
    };
    let start = run_hook("SessionStart", "session-start", false);
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    // Force the real terminal hook to seal its source into the local pending
    // namespace, then fail only the checkpoint object publication.
    let objects = fixture.repo.join(".libra/objects");
    let backup = fixture.repo.join(".libra/objects-og20-backup");
    std::fs::rename(&objects, &backup).expect("move object store aside");
    std::fs::write(&objects, b"not an object directory").expect("block object publication");
    let terminal = run_hook("SessionEnd", "session-end", true);
    std::fs::remove_file(&objects).expect("remove object-directory blocker");
    std::fs::rename(&backup, &objects).expect("restore object store");
    assert!(
        !terminal.status.success(),
        "checkpoint object publication must fail: {}",
        describe(&terminal)
    );

    let evidence_query = "SELECT scope || ':' || target || ':' || key || ':' || \
                          hex(CAST(value AS BLOB)) AS evidence FROM metadata_kv \
                          WHERE scope IN ('agent_capture_pending', 'agent_capture_pending_chunk', \
                          'agent_capture_quarantine') ORDER BY scope, target, key";
    let retained = fixture.text_rows(evidence_query, "evidence").await;
    assert!(
        retained
            .iter()
            .any(|row| row.starts_with("agent_capture_pending:")),
        "the real hook must leave an authenticated pending artifact: {} rows={retained:?}",
        describe(&terminal)
    );
    assert!(
        retained
            .iter()
            .any(|row| row.starts_with("agent_capture_pending_chunk:")),
        "the pending artifact must retain its original byte chunks"
    );

    let ordinary_source = empty_preparation_source(&fixture);
    let old_response = run_real_empty_preparation_helper(&fixture, &ordinary_source);
    assert!(
        old_response.status.success(),
        "legacy empty helper response: {}",
        describe(&old_response)
    );
    assert!(
        old_response
            .stdout
            .starts_with(b"{\"status\":\"ok\",\"projection\":")
    );
    assert!(
        !old_response
            .stdout
            .windows(9)
            .any(|bytes| bytes == b"transport")
    );
    assert_eq!(
        fixture.text_rows(evidence_query, "evidence").await,
        retained
    );

    // Unsupported transport versions are rejected by the real hidden helper
    // before its held descriptor is read or any response can be emitted.
    let mut control: Value =
        serde_json::from_str(&empty_preparation_control(1)).expect("closed legacy control");
    control["transport"] = json!({
        "version": 2,
        "challenge": vec![0_u8; 32],
        "source_preimage_commitment": vec![0_u8; 32],
        "scope_digest": vec![0_u8; 32],
        "key": vec![1_u8; 32]
    });
    let mut child = fixture
        .command()
        .arg(libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG)
        .env(
            libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV,
            control.to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn real descriptor helper with old transport version");
    let held_stdin = child.stdin.take().expect("held helper descriptor");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child
            .try_wait()
            .expect("poll rejected transport helper")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop hung transport helper");
            let _ = child.wait();
            panic!("unsupported transport version reached descriptor read");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(held_stdin);
    let rejected = child
        .wait_with_output()
        .expect("collect rejected transport helper");
    assert_eq!(rejected.status.code(), Some(2));
    assert!(
        rejected.stdout.is_empty(),
        "rejected helper emitted a frame"
    );
    assert_eq!(
        fixture.text_rows(evidence_query, "evidence").await,
        retained
    );

    // A separate real parent import attempt cannot retire the live artifact.
    let path = path_arg(&transcript);
    let import = fixture.run(&[
        "agent",
        "import",
        "--path",
        &path,
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        !import.status.success(),
        "retained live artifact should hold its original claim: {}",
        describe(&import)
    );
    assert_eq!(
        fixture.text_rows(evidence_query, "evidence").await,
        retained,
        "an ordinary helper replay must preserve the original pending artifact bytes"
    );
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

#[test]
fn agent_import_deduplicates_equivalent_transcript_working_directories() {
    let fixture = ImportRepo::init();
    let session_id = "cwd-alias-import";
    let transcript = fixture.discoverable_transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    let alias = fixture.repo.join(".");
    let body = [
        json!({
            "type": "user", "uuid": "turn-1", "sessionId": session_id,
            "cwd": fixture.repo, "message": {"role": "user", "content": "inspect"}
        }),
        json!({
            "type": "assistant", "uuid": "answer-1", "sessionId": session_id,
            "cwd": alias,
            "message": {"role": "assistant", "content": [{"type":"text", "text":"done"}]}
        }),
        json!({"type": "session_end", "sessionId": session_id, "cwd": fixture.repo}),
    ]
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(&transcript, format!("{body}\n")).expect("write alias transcript");

    let output = fixture.run(&[
        "agent",
        "import",
        "--session",
        session_id,
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "equivalent canonical transcript cwd spellings must import: {}",
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

/// ACF-08: the post-consent importer must cross the same bounded snapshot
/// boundary as live capture before it normalizes or persists any source
/// evidence. The durable checkpoint carries only the safe projection, not a
/// path or raw transcript.
#[tokio::test]
async fn import_uses_capture_foundation_services() {
    let fixture = ImportRepo::init();
    let session_id = "capture-foundation-import";
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

    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open capture-foundation db");
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, tree_oid FROM agent_checkpoint \
             WHERE session_id = 'claude__capture-foundation-import'"
                .to_string(),
        ))
        .await
        .expect("query import checkpoint")
        .expect("import checkpoint");
    let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint id");
    let tree_oid: String = row.try_get_by("tree_oid").expect("checkpoint tree");
    db.close().await.expect("close capture-foundation db");

    let metadata: Value = serde_json::from_slice(&read_checkpoint_blob(
        &fixture.repo.join(".libra"),
        &tree_oid,
        &checkpoint_id,
        &["metadata.json"],
    ))
    .expect("parse import checkpoint metadata");
    let snapshot = &metadata["transcript_snapshot"];
    assert_eq!(snapshot["completeness"], "complete");
    assert_eq!(snapshot["source"]["kind"], "trusted_export");
    assert!(
        snapshot["source"]["digest_sha256"]
            .as_str()
            .is_some_and(|digest| {
                digest.strip_prefix("source/hmac-v2/").is_some_and(|hex| {
                    hex.len() == 64
                        && hex
                            .bytes()
                            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                })
            }),
        "imported snapshot must retain only a repository-keyed HMAC v2 commitment: {snapshot}"
    );
    assert_eq!(
        snapshot["source"]["identity"], "not_retained:v1",
        "snapshot identity must not retain a correlating provider source key"
    );
    assert_eq!(metadata["redaction_report"]["snapshot_redaction"], true);
    let metadata_text = metadata.to_string();
    assert!(
        !metadata_text.contains("AKIAAAAAAAAAAAAAAAAA"),
        "the snapshot projection or report retained a raw source secret"
    );
}

/// Load the single checkpoint row a parity phase expects for `session_id`.
async fn sole_checkpoint(fixture: &ImportRepo, session_id: &str) -> (String, String) {
    let rows = fixture
        .text_rows(
            &format!(
                "SELECT checkpoint_id || ' ' || tree_oid AS row FROM agent_checkpoint \
                 WHERE session_id = '{session_id}'"
            ),
            "row",
        )
        .await;
    assert_eq!(rows.len(), 1, "expected exactly one checkpoint: {rows:?}");
    let (checkpoint_id, tree_oid) = rows[0].split_once(' ').expect("checkpoint id and tree oid");
    (checkpoint_id.to_string(), tree_oid.to_string())
}

/// The content-bearing roles of one checkpoint tree.
struct CheckpointContent {
    metadata: Value,
    transcript: Vec<u8>,
    redaction_report: Value,
}

fn checkpoint_content(
    fixture: &ImportRepo,
    (checkpoint_id, tree_oid): &(String, String),
) -> CheckpointContent {
    let storage = fixture.repo.join(".libra");
    let read = |path: &[&str]| read_checkpoint_blob(&storage, tree_oid, checkpoint_id, path);
    CheckpointContent {
        metadata: serde_json::from_slice(&read(&["metadata.json"]))
            .expect("parse checkpoint metadata"),
        transcript: read(&["transcript", "claude_code.jsonl"]),
        redaction_report: serde_json::from_slice(&read(&["redaction_report.json"]))
            .expect("parse checkpoint redaction report"),
    }
}

/// ACF-08 parity pin (AC3): identical transcript bytes captured live and
/// through `agent import` commit the same exact-source digest, snapshot
/// redaction projection and checkpoint content.
///
/// Phase 1 replays the import over the live capture: coverage recognizes the
/// turn, writes no second checkpoint, and the session-level projection keeps
/// the live digest.
///
/// Phase 2 erases the live session and imports the same bytes through the
/// audited restore path, so both entries write a checkpoint tree under one
/// repository key. Entry metadata (source kind, ids, timestamps, channel, the
/// lifecycle events and therefore `content_hash.txt`, which covers them) is
/// excluded; every content role is compared. The transcript role is typed by
/// provenance: live persists the exact redacted snapshot, import persists the
/// allowlisted per-turn projection derived from that same snapshot
/// (`CheckpointRedactedPayload::from_derived_turn_projection`). The import
/// blob must therefore be byte-for-byte the projection of the live blob, and
/// both entries must commit the same coverage-v1 content digest.
#[tokio::test]
async fn live_and_import_snapshot_hashes_match() {
    const SESSION: &str = "claude__capture-foundation-parity";
    let fixture = ImportRepo::init();
    let session_id = "capture-foundation-parity";
    let transcript = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let live = fixture.session_end_hook(session_id, &transcript);
    assert!(live.status.success(), "live hook: {}", describe(&live));

    let live_checkpoint = sole_checkpoint(&fixture, SESSION).await;
    let live_content = checkpoint_content(&fixture, &live_checkpoint);
    let live_snapshot = &live_content.metadata["transcript_snapshot"];
    assert_eq!(live_snapshot["source"]["kind"], "provider_file");
    assert_eq!(live_snapshot["completeness"], "complete");
    let live_claims = fixture
        .text_rows(
            "SELECT logical_turn_key || ' ' || coverage_digest || ' ' || completeness AS claim \
             FROM agent_coverage_claim WHERE state = 'catalog_committed' \
             ORDER BY logical_turn_key",
            "claim",
        )
        .await;
    assert!(
        !live_claims.is_empty(),
        "live capture committed no coverage"
    );

    let import_args = |restore: bool| {
        let mut args = vec![
            "agent".to_string(),
            "import".to_string(),
            "--path".to_string(),
            path_arg(&transcript),
            "--agent".to_string(),
            "claude-code".to_string(),
            "--yes".to_string(),
            "--json".to_string(),
        ];
        if restore {
            args.push("--restore-erased".to_string());
        }
        args
    };
    let covered = fixture
        .command()
        .args(import_args(false))
        .output()
        .expect("run covered import");
    assert!(
        covered.status.success(),
        "covered import: {}",
        describe(&covered)
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
            .await,
        1,
        "the import must reuse the live coverage claim rather than write another checkpoint"
    );
    let session_metadata = fixture
        .text_rows(
            &format!("SELECT metadata_json FROM agent_session WHERE session_id = '{SESSION}'"),
            "metadata_json",
        )
        .await
        .into_iter()
        .next()
        .expect("imported session metadata");
    let session_metadata: Value =
        serde_json::from_str(&session_metadata).expect("parse imported session metadata");
    let session_snapshot = &session_metadata["transcript_snapshot"];
    assert_eq!(session_snapshot["source"]["kind"], "trusted_export");
    for field in [
        "completeness",
        "transcript_redacted_bytes",
        "redaction_match_count",
        "redaction_bytes_scanned",
        "redaction_bytes_redacted",
    ] {
        assert_eq!(
            session_snapshot[field], live_snapshot[field],
            "snapshot projection field {field} drifted between live and import"
        );
    }
    assert_eq!(
        session_snapshot["source"]["digest_sha256"], live_snapshot["source"]["digest_sha256"],
        "the shared snapshot service must commit the same exact-source digest"
    );

    // Phase 2: remove the live evidence (checkpoint, catalog, coverage) and
    // import the same bytes again so the import writes its own tree.
    let conn = Database::connect(format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open writable repo db");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::try_init(libra_dir.join("objects")).expect("storage")),
        libra_dir,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local(SESSION)
        .await
        .expect("erase live session");
    assert!(erased.session_deleted);
    drop(history);
    conn.close().await.expect("close writable repo db");

    let restored = fixture
        .command()
        .args(import_args(true))
        .output()
        .expect("run restored import");
    assert!(
        restored.status.success(),
        "restored import: {}",
        describe(&restored)
    );
    let restored_json: Value = serde_json::from_slice(&restored.stdout).expect("import JSON");
    assert_eq!(restored_json["data"]["results"][0]["status"], "imported");
    let import_checkpoint = sole_checkpoint(&fixture, SESSION).await;
    assert_ne!(
        import_checkpoint.0, live_checkpoint.0,
        "the restored import must write its own checkpoint tree"
    );
    let import_content = checkpoint_content(&fixture, &import_checkpoint);

    // Source digest and snapshot projection: only the entry's source kind
    // differs (provider file versus the importer's trusted handoff).
    let mut live_projection = live_snapshot.clone();
    let mut import_projection = import_content.metadata["transcript_snapshot"].clone();
    assert_eq!(import_projection["source"]["kind"], "trusted_export");
    for projection in [&mut live_projection, &mut import_projection] {
        projection["source"]
            .as_object_mut()
            .expect("snapshot source object")
            .remove("kind");
    }
    assert_eq!(
        import_projection, live_projection,
        "checkpoint snapshot projections (digest, sizes, redaction counts) drifted"
    );

    // Transcript role: the import blob is exactly the allowlisted projection
    // of the live exact-snapshot blob, turn by turn.
    let mut derived_turns = normalize_claude_transcript(&live_content.transcript);
    redact_turns(&mut derived_turns);
    assert_eq!(derived_turns.len(), 1, "fixture holds one logical turn");
    let mut derived_blob =
        serde_json::to_vec(&safe_turn_projection("claude_code", &derived_turns[0]))
            .expect("serialize derived projection");
    derived_blob.push(b'\n');
    assert!(
        import_content.transcript == derived_blob,
        "import transcript blob is not the projection of the live snapshot blob: import={} derived={}",
        String::from_utf8_lossy(&import_content.transcript),
        String::from_utf8_lossy(&derived_blob),
    );

    // Coverage-v1 content digest: both entries commit the digest of the turn
    // derived from the live checkpoint's transcript bytes.
    let derived_claims = derived_turns
        .iter()
        .map(|turn| format!("{} {} complete", turn.logical_turn_key, turn.digest_hex()))
        .collect::<Vec<_>>();
    assert_eq!(live_claims, derived_claims, "live coverage digest drifted");
    let import_claims = fixture
        .text_rows(
            "SELECT logical_turn_key || ' ' || coverage_digest || ' ' || completeness AS claim \
             FROM agent_coverage_claim WHERE state = 'catalog_committed' \
             AND source_channel = 'import' ORDER BY logical_turn_key",
            "claim",
        )
        .await;
    assert_eq!(import_claims, live_claims, "import coverage digest drifted");

    // Redaction report: the same snapshot-stage matches and redacted byte
    // count. Import additionally records its typed-field pass and pipeline
    // markers, so its scanned-byte total may only grow.
    for key in ["matches", "bytes_redacted"] {
        assert_eq!(
            import_content.redaction_report[key], live_content.redaction_report[key],
            "redaction report {key} drifted between live and import"
        );
        assert_eq!(
            import_content.metadata["redaction_report"][key],
            live_content.metadata["redaction_report"][key],
            "metadata redaction report {key} drifted between live and import"
        );
    }
    assert!(
        live_content.redaction_report["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty()),
        "the fixture secret must produce a snapshot redaction match"
    );
    let scanned = |report: &Value| report["bytes_scanned"].as_u64().expect("bytes scanned");
    assert_eq!(
        scanned(&live_content.redaction_report),
        live_snapshot["redaction_bytes_scanned"]
            .as_u64()
            .expect("snapshot bytes scanned")
    );
    assert!(scanned(&import_content.redaction_report) >= scanned(&live_content.redaction_report));
    let extra_keys = import_content
        .redaction_report
        .as_object()
        .expect("import report object")
        .keys()
        .filter(|key| live_content.redaction_report.get(key.as_str()).is_none())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        extra_keys,
        ["pipeline", "raw_persisted", "snapshot_redaction"],
        "import redaction report gained an undocumented divergence"
    );
    for content in [&live_content, &import_content] {
        assert!(
            !String::from_utf8_lossy(&content.transcript).contains("AKIAAAAAAAAAAAAAAAAA"),
            "a checkpoint transcript retained the raw fixture secret"
        );
    }
}

#[tokio::test]
async fn agent_import_skips_live_covered_turn_and_merges_terminal_lifecycle() {
    let fixture = ImportRepo::init();
    let session_id = "live-covered-import";
    // Live capture deliberately ignores the envelope transcript pointer and
    // resolves only the provider-root-derived path. Use that discoverable
    // source so this is a real live/import coverage-dedup contract.
    let transcript = fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let live = fixture.session_end_hook(session_id, &transcript);
    assert!(live.status.success(), "live hook: {}", describe(&live));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
            .await,
        1,
        "the live side of a cross-channel dedup test must have an authorized coverage claim"
    );
    let live_coverage_digest = fixture
        .text_rows(
            "SELECT coverage_digest FROM agent_coverage_claim ORDER BY logical_turn_key",
            "coverage_digest",
        )
        .await;
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
                "SELECT coverage_digest FROM agent_coverage_claim ORDER BY logical_turn_key",
                "coverage_digest",
            )
            .await,
        live_coverage_digest,
        "the import must compare against the same canonical redacted coverage digest as live"
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
    // The subsequent live hook must read the same provider-root-derived
    // source that the explicit import consumed; an envelope pointer alone is
    // intentionally never an authorized live transcript capability.
    let transcript = fixture.write_discoverable_transcript(session_id, &fixture.repo);
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
    let import_coverage_digest = fixture
        .text_rows(
            "SELECT coverage_digest FROM agent_coverage_claim ORDER BY logical_turn_key",
            "coverage_digest",
        )
        .await;
    let import_stopped_at = fixture
        .text_rows(
            "SELECT CAST(stopped_at AS TEXT) AS stopped_at FROM agent_session \
             WHERE provider_session_id = 'import-before-live'",
            "stopped_at",
        )
        .await;

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
                "SELECT coverage_digest FROM agent_coverage_claim ORDER BY logical_turn_key",
                "coverage_digest",
            )
            .await,
        import_coverage_digest,
        "the live hook must compare against the same canonical redacted coverage digest as import"
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
    assert_eq!(
        fixture
            .text_rows(
                "SELECT CAST(stopped_at AS TEXT) AS stopped_at FROM agent_session \
                 WHERE provider_session_id = 'import-before-live'",
                "stopped_at",
            )
            .await,
        import_stopped_at,
        "a covered live replay must not re-publish the terminal timestamp"
    );
    let session_metadata = fixture
        .text_rows(
            "SELECT metadata_json FROM agent_session \
             WHERE provider_session_id = 'import-before-live'",
            "metadata_json",
        )
        .await
        .into_iter()
        .next()
        .expect("captured session metadata");
    let metadata: Value = serde_json::from_str(&session_metadata).expect("parse session metadata");
    let receipts = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("capture receipt ledger entries");
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt["status"] == "complete"),
        "covered live replay must settle its catalog receipt rather than leave finalizer work pending"
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
            "identity_scheme",
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
        assert_eq!(event["schema_version"], 2);
        assert_eq!(event["identity_scheme"], "import_uuid_v5");
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
async fn import_reactivation_consumer_contract_v1() {
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
    let terminal_sync_revision = fixture
        .scalar(
            "SELECT sync_revision AS n FROM agent_session \
             WHERE provider_session_id = 'resumed-after-end'",
        )
        .await;

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
    assert!(
        fixture
            .scalar(
                "SELECT sync_revision AS n FROM agent_session \
                 WHERE provider_session_id = 'resumed-after-end'",
            )
            .await
            > terminal_sync_revision,
        "a newer historical nonterminal tail must advance the consumer revision"
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
    let subdir = subdir.canonicalize().expect("canonical subdir");
    let transcript = fixture.write_discoverable_transcript_for_cwd("subdir123", &subdir);
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
    let output = fixture.run(&args);
    assert!(
        output.status.success(),
        "subdir import: {}",
        describe(&output)
    );
    assert_eq!(
        fixture
            .text_rows("SELECT working_dir FROM agent_session", "working_dir")
            .await,
        vec![subdir.to_string_lossy().into_owned()]
    );

    let replay = fixture.run(&args);
    assert!(
        replay.status.success(),
        "subdir import replay: {}",
        describe(&replay)
    );

    let live = fixture.session_end_hook_from_cwd(&subdir, "subdir123", &transcript);
    assert!(
        live.status.success(),
        "live hook after subdirectory adoption: {}",
        describe(&live)
    );
    assert_eq!(
        fixture
            .text_rows("SELECT working_dir FROM agent_session", "working_dir")
            .await,
        vec![subdir.to_string_lossy().into_owned()],
        "historical import must retain the verified live cwd for later hooks"
    );
}

/// A matching repository storage identity permits a live session rooted in a
/// subdirectory, but no import may adopt the same provider id from a different
/// durable capture scope.  This protects the catalog transaction-bound import
/// prepare seam from becoming a weaker replacement for `CaptureScope`.
#[tokio::test]
async fn agent_import_refuses_mismatched_catalog_scope_and_fence() {
    let provider_session_id = "foreign-scope-import";
    for label in ["repo_id", "worktree_id", "scope_state", "workspace_fence"] {
        let fixture = ImportRepo::init();
        let transcript = fixture.write_transcript(provider_session_id, &fixture.repo, true);
        let repo_id = fixture.repo_id().await;
        let (case_repo_id, worktree_id, workspace_id, workspace_fence, scope_state): (
            String,
            String,
            Option<String>,
            Option<i64>,
            &str,
        ) = match label {
            "repo_id" => (
                "foreign-repo".to_string(),
                String::new(),
                None,
                None,
                "scoped",
            ),
            "worktree_id" => (
                repo_id.clone(),
                "foreign-worktree".to_string(),
                None,
                None,
                "scoped",
            ),
            "scope_state" => (repo_id.clone(), String::new(), None, None, "legacy_unknown"),
            "workspace_fence" => (
                repo_id.clone(),
                String::new(),
                Some("import-scope-workspace".to_string()),
                Some(9_i64),
                "scoped",
            ),
            _ => unreachable!("fixed scope-mismatch matrix"),
        };
        let conn = Database::connect(format!(
            "sqlite://{}",
            fixture.repo.join(".libra/libra.db").display()
        ))
        .await
        .expect("open repo db");
        if label == "workspace_fence" {
            let workspace_path = fixture
                .repo
                .canonicalize()
                .expect("canonical workspace path")
                .to_string_lossy()
                .into_owned();
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    owner_id, task_id, session_id, base_commit, branch, state,
                    lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent', 'scope-test',
                           NULL, NULL, NULL, NULL, 'active', 'scope-test-owner',
                           8, 9999999999999, 1, 1)",
                [
                    workspace_id
                        .clone()
                        .expect("fence case has a matching workspace id")
                        .into(),
                    repo_id.clone().into(),
                    workspace_path.into(),
                ],
            ))
            .await
            .expect("seed current workspace fence");
        }
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, 'active', ?, '{}', '{}', 1, 1, 1,
                       ?, ?, ?, ?, ?)",
            [
                "claude__foreign-scope-import".into(),
                provider_session_id.into(),
                fixture.repo.to_string_lossy().into_owned().into(),
                case_repo_id.into(),
                worktree_id.into(),
                workspace_id.into(),
                workspace_fence.into(),
                scope_state.into(),
            ],
        ))
        .await
        .expect("seed foreign catalog scope");
        conn.close().await.expect("close seeded db");

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
            !output.status.success(),
            "import unexpectedly adopted {label} mismatch: {}",
            describe(&output)
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            0,
            "{label} mismatch must fail before a checkpoint is durable"
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_import_identity")
                .await,
            0,
            "{label} mismatch must fail before an import lease is durable"
        );
    }
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
    let legacy_source_fingerprint = "a".repeat(64);
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET metadata_json = ? WHERE session_id = ?",
        [
            json!({"source_fingerprint": legacy_source_fingerprint})
                .to_string()
                .into(),
            "claude__erase123".into(),
        ],
    ))
    .await
    .expect("seed legacy unkeyed source fingerprint");
    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::try_init(libra_dir.join("objects")).expect("storage")),
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
    let tombstone = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT source_fingerprint FROM agent_import_tombstone \
             WHERE agent_kind = 'claude_code' AND provider_session_id = 'erase123'"
                .to_string(),
        ))
        .await
        .expect("query erased import tombstone")
        .expect("tombstone exists");
    let tombstone_fingerprint: Option<String> = tombstone
        .try_get_by("source_fingerprint")
        .expect("decode nullable tombstone fingerprint");
    assert_eq!(
        tombstone_fingerprint, None,
        "erasure must not copy a legacy raw/unkeyed source fingerprint from session metadata into a durable tombstone"
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
async fn agent_import_tombstone_update_clears_legacy_source_fingerprint() {
    let fixture = ImportRepo::init();
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url).await.expect("open repo db");
    let legacy_source_fingerprint = "b".repeat(64);
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES (?, 'claude_code', 'tombstone-update', 'stopped', ?, ?, '{}', 1, 1, 1)",
        [
            "claude__tombstone-update".into(),
            fixture.repo.to_string_lossy().into_owned().into(),
            json!({"source_fingerprint": legacy_source_fingerprint})
                .to_string()
                .into(),
        ],
    ))
    .await
    .expect("seed session with a legacy source fingerprint");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_import_tombstone (
            tombstone_id, agent_kind, provider_session_id, erased_session_id,
            source_fingerprint, erased_at
         ) VALUES (?, 'claude_code', 'tombstone-update', ?, ?, 1)",
        [
            "legacy-tombstone-update".into(),
            "claude__previous-erasure".into(),
            "c".repeat(64).into(),
        ],
    ))
    .await
    .expect("seed pre-existing legacy tombstone");

    let libra_dir = fixture.repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::try_init(libra_dir.join("objects")).expect("storage")),
        libra_dir,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local("claude__tombstone-update")
        .await
        .expect("erase session through the existing tombstone");
    assert!(erased.session_deleted);
    drop(history);

    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT erased_session_id, source_fingerprint
             FROM agent_import_tombstone
             WHERE agent_kind = 'claude_code' AND provider_session_id = 'tombstone-update'"
                .to_string(),
        ))
        .await
        .expect("query updated tombstone")
        .expect("updated tombstone exists");
    assert_eq!(
        row.try_get_by::<String, _>("erased_session_id")
            .expect("decode erased session id"),
        "claude__tombstone-update",
        "the existing anti-resurrection barrier must continue to track the newly erased session"
    );
    let fingerprint: Option<String> = row
        .try_get_by("source_fingerprint")
        .expect("decode nullable tombstone fingerprint");
    assert_eq!(
        fingerprint, None,
        "updating an existing tombstone must clear, not retain, a legacy unkeyed source fingerprint"
    );
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
async fn import_of_a_shortened_source_releases_all_claim_leases() {
    let fixture = ImportRepo::init();
    let session_id = "shortened-source";
    let transcript = fixture.transcript_path(session_id);
    std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("create transcript directory");
    let source = [
        json!({
            "type": "user", "uuid": "shortened-turn",
            "sessionId": session_id, "cwd": fixture.repo,
            "message": {"role": "user", "content": "question"}
        }),
        json!({
            "type": "assistant", "uuid": "shortened-answer",
            "sessionId": session_id, "cwd": fixture.repo,
            "message": {"role": "assistant", "content": "answer"}
        }),
        json!({"type": "session_end", "sessionId": session_id, "cwd": fixture.repo}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(&transcript, format!("{source}\n")).expect("write shortened transcript");
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
    assert!(output.status.success(), "import: {}", describe(&output));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE state = 'reserved_import' OR owner IS NOT NULL"
            )
            .await,
        0,
        "a completed import must release every claim lease"
    );
}

#[tokio::test]
async fn doctor_retires_a_seeded_expired_object_writer_marker_without_deleting_objects() {
    let fixture = ImportRepo::init();
    let libra_dir = fixture.repo.join(".libra");
    let oid = libra::utils::object::write_git_object(&libra_dir, "blob", b"crash residue")
        .expect("write crash-residue fixture object")
        .to_string();
    let conn = Database::connect(format!("sqlite://{}", libra_dir.join("libra.db").display()))
        .await
        .expect("open writable repo db");
    let mut marker = TracesInflightMarker::new("claude__object-crash", "object-crash", 0);
    marker.ttl_ms = 0;
    marker.created_oids.push(oid.clone());
    marker.cleanup_pending = true;
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("seed durable crash ownership marker");
    conn.close().await.expect("close seeded marker database");

    let repaired = fixture.run(&["agent", "doctor", "--repair", "--json"]);
    assert!(repaired.status.success(), "{}", describe(&repaired));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await,
        0,
        "doctor repair must retire stale ownership evidence"
    );
    let object_path = libra_dir.join("objects").join(&oid[..2]).join(&oid[2..]);
    assert!(
        object_path.exists(),
        "ownership retirement is non-destructive; repository GC owns loose object reclamation"
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
    let added = fixture.run(&[
        "worktree",
        "add",
        "sibling-worktree",
        "--path",
        sibling_arg.as_str(),
    ]);
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
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-018"),
        "the discovery safety bound must fail loudly: {}",
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
    // SHA-256("batchbad")[..6]: the documented short hashed session id.
    assert!(
        stderr.contains("\"session_id\": \"sha256:33cec74cbb24\""),
        "{}",
        describe(&partial)
    );
    assert!(
        !stderr.contains("source/hmac-v2") && !stderr.contains("\"session_id\": \"batchbad\""),
        "batch failures must not expose a source commitment or raw session id: {}",
        describe(&partial)
    );
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
    // SHA-256("123e4567-e89b-12d3-a456-426614174011")[..6].
    assert_eq!(
        payload["data"]["skipped"][0]["session_id"],
        "sha256:278a7e727b81"
    );
    assert_eq!(
        payload["data"]["failures"].as_array().map(Vec::len),
        Some(0)
    );
}

/// Discovery runs in a private helper; argv errors are rejected by the parent
/// and provider errors cross the wire only as a closed reason, but each must
/// still render the shipped actionable message, stable code, and exit status.
#[test]
fn agent_import_discovery_errors_keep_actionable_messages() {
    let fixture = ImportRepo::init();
    // A real Claude project root, so a missing id is "not found" rather than
    // an absent provider.
    fixture.write_discoverable_transcript("present", &fixture.repo);
    for (args, message, code, exit) in [
        (
            &[
                "--all",
                "--agent",
                "claude-code",
                "--limit",
                "0",
                "--yes",
                "--json",
            ][..],
            "--limit must be between 1 and 100",
            "LBR-CLI-002",
            129,
        ),
        (
            &["--all", "--agent", "gemini", "--yes"][..],
            "agent import supports claude-code, codex, opencode, hermes, or pi; got 'gemini'",
            "LBR-CLI-002",
            129,
        ),
        (
            &["--path", "x.jsonl", "--yes"][..],
            "--path requires --agent",
            "LBR-CLI-002",
            129,
        ),
        (
            &["--all", "--agent", "opencode", "--yes"][..],
            "OpenCode batch discovery is unavailable; select a session explicitly with --session",
            "LBR-CLI-002",
            129,
        ),
        (
            &[
                "--session",
                "missing123",
                "--agent",
                "claude-code",
                "--yes",
                "--json",
            ][..],
            "no authorized local transcript matched the session id; use --agent opencode for an export-only OpenCode session",
            "LBR-CLI-003",
            129,
        ),
        (
            &["--all", "--agent", "claude-code", "--cursor", "5", "--yes"][..],
            "--cursor is outside the discovery result set",
            "LBR-CLI-002",
            129,
        ),
    ] {
        let output = fixture
            .command()
            .args(["agent", "import"])
            .args(args)
            .output()
            .expect("run agent import");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}: {}",
            describe(&output)
        );
        assert!(stderr.contains(message), "{args:?}: {}", describe(&output));
        assert!(stderr.contains(code), "{args:?}: {}", describe(&output));
        assert!(
            !stderr.contains("rejected the supplied selector")
                && !stderr.contains("rejected the selected target"),
            "{args:?}: {}",
            describe(&output)
        );
    }

    // The same UUID as a Claude transcript and a Codex rollout is ambiguous
    // without --agent.
    let session_id = "123e4567-e89b-12d3-a456-426614174099";
    fixture.write_discoverable_transcript(session_id, &fixture.repo);
    let codex_home = fixture._tmp.path().join("codex-home");
    let day = codex_home.join("sessions/2026/07/15");
    std::fs::create_dir_all(&day).expect("create Codex day partition");
    std::fs::write(
        day.join(format!("rollout-2026-07-15T01-00-00-{session_id}.jsonl")),
        format!(
            "{}\n",
            json!({
                "type": "session_meta",
                "timestamp": "2026-07-15T01:00:00Z",
                "payload": {"id": session_id, "cwd": fixture.repo}
            })
        ),
    )
    .expect("write Codex rollout");
    let ambiguous = fixture
        .command()
        .env("CODEX_HOME", &codex_home)
        .args(["agent", "import", "--session", session_id, "--yes"])
        .output()
        .expect("run ambiguous agent import");
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert_eq!(
        ambiguous.status.code(),
        Some(129),
        "{}",
        describe(&ambiguous)
    );
    assert!(
        stderr.contains("the session id matches multiple providers; add --agent")
            && stderr.contains("LBR-CLI-002"),
        "{}",
        describe(&ambiguous)
    );
}

#[tokio::test]
async fn agent_import_all_records_each_completed_candidate() {
    let fixture = ImportRepo::init();
    fixture.write_discoverable_transcript("a-index-first", &fixture.repo);
    fixture.write_discoverable_transcript("b-index-second", &fixture.repo);
    let output = fixture.run(&[
        "agent",
        "import",
        "--all",
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{}", describe(&output));
    let payload: Value = serde_json::from_slice(&output.stdout).expect("batch JSON");
    assert_eq!(payload["data"]["results"].as_array().map(Vec::len), Some(2));
    assert!(
        payload["data"]["results"]
            .as_array()
            .expect("batch results")
            .iter()
            .all(|result| result["status"] == "imported"),
        "each discovered candidate should report imported: {}",
        describe(&output)
    );
    assert_eq!(
        payload["data"]["failures"].as_array().map(Vec::len),
        Some(0)
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
        1,
        "each discovered candidate should receive its own checkpoint"
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__a-index-first'"
            )
            .await,
        0,
        "a completed import should not retain an index-repair barrier"
    );
}

#[tokio::test]
async fn agent_import_completed_index_write_needs_no_repair_barrier() {
    let fixture = ImportRepo::init();
    let transcript = fixture.write_discoverable_transcript("index-error", &fixture.repo);
    let transcript_arg = path_arg(&transcript);
    let first = fixture.run(&[
        "agent",
        "import",
        "--path",
        transcript_arg.as_str(),
        "--agent",
        "claude-code",
        "--yes",
        "--json",
    ]);
    assert!(first.status.success(), "{}", describe(&first));
    assert_eq!(
        fixture
            .scalar(
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_import_index_repair'
                   AND target = 'claude__index-error'"
            )
            .await,
        0,
        "a completed import should not retain a repair barrier"
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
    let doctor = fixture.run(&["agent", "doctor", "--json"]);
    assert!(doctor.status.success(), "{}", describe(&doctor));
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("missing_object_index"),
        "foreground replay repair must restore every E4 index row: {}",
        describe(&doctor)
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

/// Explicit direct prepared turns exercise the actual local DB writer, with no
/// provider process, exported Session, credentials or model response.
#[cfg(unix)]
#[tokio::test]
async fn cov2_mixed_v1_v2_direct_import_idempotent() {
    use libra::internal::ai::{
        agent_import::{ImportRequest, TurnBoundary, import_prepared_with_coverage_versions},
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotProjection, CaptureSnapshotSource,
            CaptureSnapshotSourceKind,
        },
        capture_scope::CaptureScope,
        observed_agents::{Completeness, NormalizedTurn, SemanticRecord},
    };

    let fixture = ImportRepo::init();
    let storage = fixture.repo.join(".libra");
    let conn = Database::connect(format!("sqlite://{}", storage.join("libra.db").display()))
        .await
        .unwrap();
    let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
    let turn = |key: &str, ordinal: usize| NormalizedTurn {
        logical_turn_key: key.to_owned(),
        ordinal,
        completeness: Completeness::Complete,
        started_at: None,
        ended_at: None,
        records: vec![SemanticRecord::User {
            text: "fixed ordinary direct import".to_owned(),
        }],
    };
    let request = |turns: Vec<NormalizedTurn>| {
        let commitment = format!("source/hmac-v2/{}", "a".repeat(64));
        ImportRequest {
            agent_kind: AgentKind::Codex,
            provider_session_id: "cov2-direct-integration".to_owned(),
            session_id: "codex__cov2-direct-integration".to_owned(),
            source_kind: "file".to_owned(),
            source_id: commitment.clone(),
            identity_schema_version: 2,
            content_digest: "caller digest must not be trusted".to_owned(),
            started_at: 1,
            ended_at: 2,
            session_state: "stopped".to_owned(),
            stopped_at: Some(2),
            working_dir: fixture.repo.clone(),
            repository_identity: "not_retained:v1".to_owned(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: commitment.clone(),
            existing_session_fingerprint: None,
            redaction_report: json!({"pipeline":"typed_allowlist","raw_persisted":false,"matches":[],"bytes_scanned":32,"bytes_redacted":0}),
            transcript_snapshot: Some(CaptureSnapshotProjection {
                completeness: CaptureSnapshotCompleteness::Complete,
                partial_reason: None,
                source: Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: "not_retained:v1".to_owned(),
                    digest_sha256: Some(commitment),
                    byte_len: Some(32),
                }),
                transcript_redacted_bytes: 32,
                redaction_match_count: 0,
                redaction_bytes_scanned: 32,
                redaction_bytes_redacted: 0,
            }),
            turn_boundaries: turns
                .iter()
                .map(|turn| {
                    (
                        turn.logical_turn_key.clone(),
                        TurnBoundary {
                            started_at: 1,
                            ended_at: 2,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            turns,
        }
    };
    // Reverse lexical/source order catches a sidecar left behind by sorting.
    let v1 = vec![turn("z-first", 0)];
    let mixed = vec![turn("z-first", 0), turn("a-second", 1)];
    for (turns, versions, written, skipped) in [
        (v1.clone(), vec![1], 1, 0),
        (mixed.clone(), vec![1, 2], 1, 1),
        (v1.clone(), vec![1], 0, 1),
        (v1.clone(), vec![2], 1, 0),
        (v1.clone(), vec![2], 0, 1),
        (mixed.clone(), vec![1, 2], 0, 2),
        (vec![mixed[1].clone(), mixed[0].clone()], vec![2, 1], 0, 2),
    ] {
        let summary = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            request(turns),
            &versions,
            Duration::from_secs(30),
            Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
        )
        .await
        .unwrap();
        assert_eq!(summary.checkpoints_written, written);
        assert_eq!(summary.skipped_covered, skipped);
        assert!(!summary.partial);
    }
    let rows = conn.query_all_raw(Statement::from_string(conn.get_database_backend(),
        "SELECT logical_turn_key, coverage_schema_version, revision FROM agent_coverage_claim ORDER BY logical_turn_key, coverage_schema_version".to_owned())).await.unwrap();
    let actual = rows
        .iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("logical_turn_key").unwrap(),
                row.try_get_by::<i64, _>("coverage_schema_version").unwrap(),
                row.try_get_by::<i64, _>("revision").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            ("a-second".to_owned(), 2, 1),
            ("z-first".to_owned(), 1, 1),
            ("z-first".to_owned(), 2, 1)
        ]
    );
    let checkpoints = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT r.logical_turn_key, r.coverage_schema_version, c.checkpoint_id, c.tree_oid
         FROM agent_coverage_revision r JOIN agent_checkpoint c ON c.checkpoint_id=r.checkpoint_id"
                .to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(checkpoints.len(), 3);
    for row in checkpoints {
        let key: String = row.try_get_by("logical_turn_key").unwrap();
        let version: i64 = row.try_get_by("coverage_schema_version").unwrap();
        let id: String = row.try_get_by("checkpoint_id").unwrap();
        let tree: String = row.try_get_by("tree_oid").unwrap();
        let transcript = read_checkpoint_blob(&storage, &tree, &id, &["transcript", "codex.jsonl"]);
        let projection: Value = serde_json::from_slice(&transcript).unwrap();
        assert_eq!(projection["schema_version"], version);
        assert_eq!(projection["logical_turn_key"], key);
    }
    let count = |table: &str| {
        let query = format!("SELECT COUNT(*) AS n FROM {table}");
        let conn = &conn;
        async move {
            conn.query_one_raw(Statement::from_string(conn.get_database_backend(), query))
                .await
                .unwrap()
                .unwrap()
                .try_get_by::<i64, _>("n")
                .unwrap()
        }
    };
    assert_eq!(count("agent_checkpoint").await, 3);
    assert_eq!(count("agent_coverage_revision").await, 3);
    let before = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT fence_token, committed_digest FROM agent_import_identity".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let fence: i64 = before.try_get_by("fence_token").unwrap();
    let digest: String = before.try_get_by("committed_digest").unwrap();
    assert_ne!(digest, "caller digest must not be trusted");
    for (turns, versions) in [
        (vec![turn("new", 0)], vec![3]),
        (vec![turn("new", 0)], vec![]),
        (vec![turn("same", 0), turn("same", 1)], vec![1, 2]),
    ] {
        assert!(
            import_prepared_with_coverage_versions(
                &conn,
                &storage,
                request(turns),
                &versions,
                Duration::from_secs(30),
                Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
            )
            .await
            .is_err()
        );
    }
    let canary = format!("AKIA{}", "A".repeat(16));
    let mut secret = turn("private", 0);
    secret.records = vec![SemanticRecord::User {
        text: canary.clone(),
    }];
    let error = import_prepared_with_coverage_versions(
        &conn,
        &storage,
        request(vec![secret]),
        &[2],
        Duration::from_secs(30),
        Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
    )
    .await
    .unwrap_err();
    assert!(!format!("{error:#}").contains(&canary));
    for budget in [Duration::ZERO, Duration::from_secs(301)] {
        assert!(
            import_prepared_with_coverage_versions(
                &conn,
                &storage,
                request(v1.clone()),
                &[1],
                budget,
                Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
            )
            .await
            .is_err()
        );
    }
    let mut foreign_scope = request(v1.clone());
    foreign_scope.capture_scope.as_mut().unwrap().repo_id = "different-repository".to_owned();
    assert!(
        import_prepared_with_coverage_versions(
            &conn,
            &storage,
            foreign_scope,
            &[1],
            Duration::from_secs(30),
            Some(Path::new(env!("CARGO_BIN_EXE_libra")))
        )
        .await
        .is_err()
    );
    let foreign_storage = fixture.repo.join("foreign-storage");
    assert!(
        import_prepared_with_coverage_versions(
            &conn,
            &foreign_storage,
            request(v1.clone()),
            &[1],
            Duration::from_secs(30),
            Some(Path::new(env!("CARGO_BIN_EXE_libra")))
        )
        .await
        .is_err()
    );
    assert!(
        !foreign_storage.exists(),
        "storage mismatch must reject before any filesystem writes"
    );
    let absent_helper = fixture.repo.join("missing-libra-helper");
    for helper in [
        None,
        Some(Path::new("relative-libra")),
        Some(absent_helper.as_path()),
    ] {
        let error = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            request(v1.clone()),
            &[1],
            Duration::from_secs(30),
            helper,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("checkpoint helper"));
    }
    assert_eq!(count("agent_checkpoint").await, 3);
    assert_eq!(count("agent_coverage_claim").await, 3);
    let after = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT fence_token FROM agent_import_identity".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.try_get_by::<i64, _>("fence_token").unwrap(),
        fence,
        "invalid input must reject before leasing"
    );
}

fn hermes_source_scope(
    repo: &std::path::Path,
) -> libra::internal::ai::observed_agents::HermesScope {
    libra::internal::ai::observed_agents::HermesScope {
        session_id: "sess-hermes".to_string(),
        profile: "default".to_string(),
        repo_root: repo.to_path_buf(),
    }
}

fn hermes_source_session(repo: &std::path::Path, messages: serde_json::Value) -> String {
    serde_json::json!({
        "id": "sess-hermes",
        "source": "cli",
        "created_source": "cli",
        "user_id": null,
        "cwd": repo,
        "profile_name": "default",
        "messages": messages,
    })
    .to_string()
}

#[test]
fn hermes_source_jsonl_keeps_thinking_and_drops_revoked_rows() {
    let repo = std::path::PathBuf::from("/work/repo");
    let messages = serde_json::json!([
        {"active": 0, "compacted": 0, "reasoning": "revoked-thought", "reasoning_content": null},
        {"active": 1, "compacted": 0, "reasoning": null, "reasoning_content": "", "reasoning_details": "{\"raw\":1}", "codex_reasoning_items": "[]", "codex_message_items": null},
        {"active": 0, "compacted": 1, "reasoning": "kept-thought", "reasoning_content": "also-kept", "reasoning_details": "{\"keep\":true}"}
    ]);
    let bytes = hermes_source_session(&repo, messages);
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        bytes.as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(transcript) = status else {
        panic!("jsonl read failed: {status}");
    };
    assert!(transcript.thinking_fields_present);
    assert!(!transcript.partial);
    assert_eq!(transcript.messages.len(), 2, "revoked row must stay out");
    assert!(
        transcript
            .messages
            .iter()
            .all(|message| message.readable_thinking() != Some("revoked-thought"))
    );
    assert!(transcript.messages[0].readable_thinking().is_none());
    assert_eq!(transcript.messages[0].reasoning, None);
    assert_eq!(
        transcript.messages[0].reasoning_content.as_deref(),
        Some("")
    );
    assert_eq!(
        transcript.messages[0].reasoning_details.as_deref(),
        Some("{\"raw\":1}")
    );
    assert_eq!(
        transcript.messages[0].codex_reasoning_items.as_deref(),
        Some("[]")
    );
    assert_eq!(transcript.messages[0].codex_message_items, None);
    assert_eq!(
        transcript.messages[1].readable_thinking(),
        Some("kept-thought")
    );
    assert_eq!(
        transcript.messages[1].reasoning_content.as_deref(),
        Some("also-kept")
    );
    assert_eq!(
        transcript.messages[1].reasoning_details.as_deref(),
        Some("{\"keep\":true}")
    );
    assert!(!status_text_leaks(&transcript.messages[1]));

    let limits = libra::internal::ai::observed_agents::HermesReadLimits {
        max_rows: 1,
        ..libra::internal::ai::observed_agents::HermesReadLimits::default()
    };
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        bytes.as_bytes(),
        &hermes_source_scope(&repo),
        &limits,
    );
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(truncated) = status else {
        panic!("row budget read failed: {status}");
    };
    assert!(
        truncated.partial,
        "a kept row past the budget must stay partial"
    );
    assert_eq!(truncated.messages.len(), 1);
}

#[test]
fn hermes_source_jsonl_accepts_the_local_transport_label() {
    let repo = std::path::PathBuf::from("/work/repo");
    let mut local = serde_json::from_str::<serde_json::Value>(&hermes_source_session(
        &repo,
        serde_json::json!([
            {"active": 1, "compacted": 0, "reasoning": "snake-thought", "reasoning_content": "snake-thought"}
        ]),
    ))
    .expect("json");
    local["source"] = serde_json::json!("local");
    local["created_source"] = serde_json::json!("cli");
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        local.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(transcript) = status else {
        panic!("source=local created_source=cli must stay readable: {status}");
    };
    assert_eq!(
        transcript.messages[0].readable_thinking(),
        Some("snake-thought")
    );
    local["created_source"] = serde_json::json!("telegram");
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        local.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));
}

#[cfg(unix)]
#[test]
fn hermes_source_jsonl_accepts_only_the_local_uid_subject() {
    let repo = std::path::PathBuf::from("/work/repo");
    let mut local = serde_json::from_str::<serde_json::Value>(&hermes_source_session(
        &repo,
        serde_json::json!([
            {"active": 1, "compacted": 0, "reasoning": "local-thought", "reasoning_content": "local-thought"}
        ]),
    ))
    .expect("json");
    // SAFETY: geteuid only reads the process credential.
    let uid = unsafe { libc::geteuid() };
    local["user_id"] = serde_json::json!(format!("uid:{uid}"));
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        local.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(transcript) = status else {
        panic!("local uid must stay readable: {status}");
    };
    assert_eq!(
        transcript.messages[0].readable_thinking(),
        Some("local-thought")
    );

    local["user_id"] = serde_json::json!("uid:0");
    if uid == 0 {
        local["user_id"] = serde_json::json!("uid:1");
    }
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        local.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));
    local["user_id"] = serde_json::json!("telegram-user");
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        local.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));
    assert!(!status.to_string().contains("local-thought"));
}

#[test]
fn hermes_source_jsonl_rejects_gateway_outside_cwd_and_symlink() {
    let root = tempfile::tempdir().expect("root");
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).expect("repo");
    let gateway = hermes_source_session(
        &repo,
        serde_json::json!([
            {"active": 1, "compacted": 0, "reasoning": "nope"}
        ]),
    );
    let mut gateway_value: serde_json::Value = serde_json::from_str(&gateway).expect("json");
    gateway_value["source"] = serde_json::json!("gateway");
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        gateway_value.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));
    assert!(!status.to_string().contains("nope"));
    gateway_value["source"] = serde_json::json!("matrix");
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        gateway_value.to_string().as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));

    let outside = hermes_source_session(
        std::path::Path::new("/other/repo"),
        serde_json::json!([
            {"active": 1, "compacted": 0, "reasoning": "outside-thought"}
        ]),
    );
    let status = libra::internal::ai::observed_agents::read_hermes_jsonl(
        outside.as_bytes(),
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    );
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));

    #[cfg(unix)]
    {
        let export_dir = root.path().join("export");
        std::fs::create_dir(&export_dir).expect("export dir");
        let real = export_dir.join("real.jsonl");
        std::fs::write(&real, format!("{gateway}\n")).expect("real");
        let link = export_dir.join("link.jsonl");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let status = libra::internal::ai::observed_agents::read_hermes_jsonl_file(
            &link,
            &export_dir,
            &hermes_source_scope(&repo),
            &libra::internal::ai::observed_agents::HermesReadLimits::default(),
        );
        assert!(matches!(
            status,
            libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
        ));
        assert!(!status.to_string().contains(&real.display().to_string()));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn hermes_source_sqlite_reads_wal_and_refuses_a_read_only_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("root");
    let root = root.path().canonicalize().expect("canonical root");
    let repo = root.join("repo");
    std::fs::create_dir(&repo).expect("repo");
    let db = root.join("state.db");
    let script = format!(
        r#"
import os
import sqlite3
conn = sqlite3.connect({db:?})
conn.execute("PRAGMA journal_mode=WAL")
conn.execute("CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, created_source TEXT, user_id TEXT, cwd TEXT, profile_name TEXT)")
conn.execute("CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, reasoning TEXT, reasoning_content TEXT, reasoning_details TEXT, codex_reasoning_items TEXT, codex_message_items TEXT, active INTEGER NOT NULL, compacted INTEGER NOT NULL)")
conn.execute("INSERT INTO sessions VALUES ('sess-hermes', 'cli', 'cli', NULL, ?, 'default')", ({repo:?},))
conn.execute("INSERT INTO messages (session_id, reasoning, reasoning_content, reasoning_details, codex_reasoning_items, codex_message_items, active, compacted) VALUES ('sess-hermes', 'revoked-thought', NULL, NULL, NULL, NULL, 0, 0)")
conn.execute("INSERT INTO messages (session_id, reasoning, reasoning_content, reasoning_details, codex_reasoning_items, codex_message_items, active, compacted) VALUES ('sess-hermes', NULL, '', '{{\"raw\":1}}', '[]', NULL, 1, 0)")
conn.execute("INSERT INTO messages (session_id, reasoning, reasoning_content, reasoning_details, codex_reasoning_items, codex_message_items, active, compacted) VALUES ('sess-hermes', 'kept-thought', NULL, NULL, NULL, NULL, 0, 1)")
conn.commit()
# Exit without closing the connection: a clean close checkpoints and deletes
# the WAL, and whether interpreter shutdown closes it depends on the Python
# version.
os._exit(0)
"#,
        db = db.display().to_string(),
        repo = repo.display().to_string(),
    );
    let planted = std::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .status()
        .expect("python");
    assert!(planted.success(), "sqlite fixture");
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    assert!(wal.exists(), "wal must hold the uncheckpointed rows");
    let db_before = std::fs::read(&db).expect("db bytes");
    let wal_before = std::fs::read(&wal).expect("wal bytes");
    let request = libra::internal::ai::observed_agents::HermesSourceRequest {
        authorized_root: root.clone(),
        jsonl: None,
        sqlite: Some(db.clone()),
        scope: hermes_source_scope(&repo),
        limits: libra::internal::ai::observed_agents::HermesReadLimits::default(),
    };
    let status = libra::internal::ai::observed_agents::read_hermes_source(&request).await;
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(transcript) = &status else {
        panic!("wal read failed: {status}");
    };
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(
        transcript.messages[1].readable_thinking(),
        Some("kept-thought")
    );
    let tight = libra::internal::ai::observed_agents::HermesReadLimits {
        max_rows: 1,
        ..libra::internal::ai::observed_agents::HermesReadLimits::default()
    };
    let tight_status = libra::internal::ai::observed_agents::read_hermes_sqlite(
        &db,
        &root,
        &hermes_source_scope(&repo),
        &tight,
    )
    .await;
    let libra::internal::ai::observed_agents::HermesSourceStatus::Read(tight_transcript) =
        &tight_status
    else {
        panic!("sqlite row budget failed: {tight_status}");
    };
    assert!(
        tight_transcript.partial,
        "revoked rows must not hide a kept row past the budget"
    );
    assert_eq!(tight_transcript.messages.len(), 1);
    assert!(
        transcript
            .messages
            .iter()
            .all(|message| message.readable_thinking() != Some("revoked-thought"))
    );
    assert_eq!(std::fs::read(&db).expect("db after"), db_before);
    assert_eq!(std::fs::read(&wal).expect("wal after"), wal_before);
    assert!(!status.to_string().contains("kept-thought"));
    assert!(!status.to_string().contains("SELECT"));

    let shm = PathBuf::from(format!("{}-shm", db.display()));
    let _ = std::fs::remove_file(&shm);
    let status = libra::internal::ai::observed_agents::read_hermes_sqlite(
        &db,
        &root,
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    )
    .await;
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Read(_)
    ));
    assert_eq!(std::fs::read(&db).expect("db after shm"), db_before);
    assert_eq!(std::fs::read(&wal).expect("wal after shm"), wal_before);

    let _ = std::fs::remove_file(&shm);
    let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", db.display())));
    let mut locked = std::fs::metadata(&root).expect("root meta").permissions();
    locked.set_mode(0o555);
    std::fs::set_permissions(&root, locked).expect("readonly dir");
    let status = libra::internal::ai::observed_agents::read_hermes_sqlite(
        &db,
        &root,
        &hermes_source_scope(&repo),
        &libra::internal::ai::observed_agents::HermesReadLimits::default(),
    )
    .await;
    let mut restore = std::fs::metadata(&root).expect("root meta").permissions();
    restore.set_mode(0o755);
    std::fs::set_permissions(&root, restore).expect("restore dir");
    assert!(matches!(
        status,
        libra::internal::ai::observed_agents::HermesSourceStatus::Unavailable
    ));
    assert_eq!(std::fs::read(&db).expect("db readonly"), db_before);
    assert_eq!(std::fs::read(&wal).expect("wal readonly"), wal_before);
    assert!(!status.to_string().contains(&db.display().to_string()));
}

fn status_text_leaks(message: &libra::internal::ai::observed_agents::HermesMessage) -> bool {
    let rendered = format!("{message:?}");
    rendered.contains("/secret")
}

fn hermes_normalize_session(repo: &std::path::Path, messages: serde_json::Value) -> String {
    json!({
        "id": "0123456789abcdef0123456789abcdef",
        "source": "cli",
        "cwd": repo,
        "started_at": 1_700_000_000,
        "ended_at": 1_700_000_100,
        "parent_session_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "input_tokens": 5,
        "output_tokens": 7,
        "session_model_usage": [{"input_tokens": 50, "output_tokens": 70}],
        "messages": messages,
    })
    .to_string()
}

fn hermes_normalize_scope(
    repo: &std::path::Path,
) -> libra::internal::ai::observed_agents::builtin::hermes::HermesScope {
    libra::internal::ai::observed_agents::builtin::hermes::HermesScope {
        session_id: "0123456789abcdef0123456789abcdef".to_string(),
        profile: "default".to_string(),
        repo_root: repo.to_path_buf(),
    }
}

#[test]
fn hermes_normalize_sorts_by_id_keeps_thinking_and_ignores_mirrored_usage() {
    use libra::internal::ai::observed_agents::{
        Completeness, HermesReadLimits, SemanticRecord,
        builtin::hermes::{HermesNormalizeStatus, normalize_hermes_jsonl},
    };

    let repo = std::env::temp_dir().join("hermes-normalize-scope");
    let bytes = hermes_normalize_session(
        &repo,
        json!([
            {"id": 3, "role": "assistant", "content": "answer", "active": 1, "compacted": 0,
             "timestamp": 10, "reasoning": "kept-thinking", "reasoning_content": "",
             "reasoning_details": "{\"raw\":true}", "token_count": 99, "finish_reason": "stop",
             "tool_calls": [{"id": "call-1", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}]},
            {"id": 1, "role": "user", "content": "prompt", "active": 1, "compacted": 0,
             "timestamp": 99, "reasoning": null},
            {"id": 2, "role": "user", "content": "revoked", "active": 0, "compacted": 0,
             "timestamp": 1, "token_count": 99},
            {"id": 4, "role": "tool", "content": "tool-out", "tool_call_id": "call-1",
             "active": 1, "compacted": 0, "timestamp": 11, "token_count": 5},
            {"id": 5, "role": "assistant", "content": "failed-copy", "active": 1, "compacted": 0,
             "timestamp": 12, "finish_reason": "error", "token_count": 40, "message_uid": "dup"},
            {"id": 6, "role": "assistant", "content": "retried-answer", "active": 1, "compacted": 0,
             "timestamp": 13, "finish_reason": "stop", "token_count": 40, "message_uid": "dup"},
            {"id": 7, "role": "assistant", "content": "truncated", "active": 1, "compacted": 0,
             "timestamp": 14, "finish_reason": "length", "token_count": 3}
        ]),
    );
    let status = normalize_hermes_jsonl(
        bytes.as_bytes(),
        &hermes_normalize_scope(&repo),
        &HermesReadLimits::default(),
    );
    let HermesNormalizeStatus::Ready(normalized) = status else {
        panic!("normalize unavailable");
    };
    assert!(!normalized.parent_associated);
    assert_eq!(normalized.source_state, "unproven");
    let usage = normalized.usage.expect("known session usage");
    assert_eq!(usage.input_tokens, Some(5));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.cache_read_tokens, None);
    assert!(!usage.malformed);
    assert_eq!(normalized.turns.len(), 1);
    let turn = &normalized.turns[0];
    assert_eq!(turn.logical_turn_key, "1");
    assert_eq!(turn.ordinal, 0);
    assert_eq!(turn.completeness, Completeness::Incomplete);
    assert!(matches!(turn.records[0], SemanticRecord::User { ref text } if text == "prompt"));
    assert!(matches!(turn.records[1], SemanticRecord::Assistant { ref text } if text == "answer"));
    assert!(
        turn.records.iter().any(
            |record| matches!(record, SemanticRecord::ToolCall { name, .. } if name == "read")
        )
    );
    assert!(turn.records.iter().any(|record| matches!(record, SemanticRecord::ToolResult { content, .. } if content == "tool-out")));
    assert!(turn.records.iter().any(
        |record| matches!(record, SemanticRecord::Assistant { text } if text == "retried-answer")
    ));
    assert!(!format!("{turn:?}").contains("kept-thinking"));
    assert!(!format!("{turn:?}").contains("failed-copy"));
    assert!(!format!("{turn:?}").contains("revoked"));
    let retained = normalized
        .retained
        .iter()
        .find(|item| item.message_id == Some(3))
        .expect("retained thinking");
    assert_eq!(retained.reasoning.as_deref(), Some("kept-thinking"));
    assert_eq!(retained.reasoning_content.as_deref(), Some(""));
    assert_eq!(
        retained.reasoning_details.as_deref(),
        Some("{\"raw\":true}")
    );
    assert_eq!(retained.turn_key, "1");

    let unknown = hermes_normalize_session(
        &repo,
        json!([
            {"id": 1, "role": "user", "content": "prompt", "active": 1, "compacted": 0, "timestamp": 10}
        ]),
    );
    let unknown = unknown
        .replace("\"input_tokens\":5,", "")
        .replace("\"output_tokens\":7,", "");
    let status = normalize_hermes_jsonl(
        unknown.as_bytes(),
        &hermes_normalize_scope(&repo),
        &HermesReadLimits::default(),
    );
    let HermesNormalizeStatus::Ready(normalized) = status else {
        panic!("unknown usage unavailable");
    };
    assert!(normalized.usage.is_none());
}

#[test]
fn hermes_normalize_import_replays_one_checkpoint_without_enabling_hermes() {
    use libra::internal::ai::observed_agents::registration_for;

    let fixture = ImportRepo::init();
    let session_id = "0123456789abcdef0123456789abcdef";
    let path = fixture.repo.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &path,
        hermes_normalize_session(
            &fixture.repo,
            json!([
                {"id": 1, "role": "user", "content": "prompt", "active": 1, "compacted": 0, "timestamp": 1700000001},
                {"id": 2, "role": "assistant", "content": "answer", "active": 1, "compacted": 0,
                 "timestamp": 1700000002, "reasoning": "kept-thinking", "finish_reason": "stop"}
            ]),
        ),
    )
    .expect("write export");
    let scanned = fixture.run(&["agent", "import", "--agent", "hermes", "--all", "--yes"]);
    assert!(!scanned.status.success(), "{}", describe(&scanned));
    assert_eq!(checkpoint_count(&fixture), 0);
    let first = fixture.run(&[
        "agent",
        "import",
        "--agent",
        "hermes",
        "--path",
        path.to_str().expect("utf8 path"),
        "--yes",
    ]);
    assert!(first.status.success(), "{}", describe(&first));
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!shown.contains("kept-thinking"), "{shown}");
    assert!(!shown.contains("prompt"), "{shown}");
    assert_eq!(checkpoint_count(&fixture), 1);
    let state = identity_state(&fixture);
    assert_eq!(state, "committed");
    let second = fixture.run(&[
        "agent",
        "import",
        "--agent",
        "hermes",
        "--path",
        path.to_str().expect("utf8 path"),
        "--yes",
    ]);
    assert!(second.status.success(), "{}", describe(&second));
    assert_eq!(
        checkpoint_count(&fixture),
        1,
        "replay wrote another checkpoint"
    );
    let registration = registration_for(AgentKind::Hermes);
    assert!(registration.supported);
    assert!(registration.hook_installable);
    assert!(!registration.launchable_review);
    assert!(!registration.launchable_investigate);
    assert!(registration.transcript_readable);
    let listed = fixture.run(&["agent", "list", "--json"]);
    assert!(listed.status.success(), "{}", describe(&listed));
    assert!(String::from_utf8_lossy(&listed.stdout).contains("hermes"));

    let wrong = fixture.repo.join("fedcba9876543210fedcba9876543210.jsonl");
    let mut body = hermes_normalize_session(
        &fixture.repo,
        json!([
            {"id": 1, "role": "user", "content": "other", "active": 1, "compacted": 0, "timestamp": 1700000001}
        ]),
    );
    body = body.replace(
        "\"id\":\"0123456789abcdef0123456789abcdef\"",
        "\"id\":\"fedcba9876543210fedcba9876543210\"",
    );
    body = body.replace(
        "\"source\":\"cli\"",
        "\"source\":\"cli\",\"profile_name\":\"other\"",
    );
    std::fs::write(&wrong, body).expect("write other profile");
    let rejected = fixture.run(&[
        "agent",
        "import",
        "--agent",
        "hermes",
        "--path",
        wrong.to_str().expect("utf8 path"),
        "--yes",
    ]);
    assert!(!rejected.status.success(), "{}", describe(&rejected));
    assert_eq!(checkpoint_count(&fixture), 1);
}

fn checkpoint_count(fixture: &ImportRepo) -> i64 {
    scalar(fixture, "SELECT COUNT(*) FROM agent_checkpoint")
}

fn identity_state(fixture: &ImportRepo) -> String {
    scalar_text(fixture, "SELECT state FROM agent_import_identity")
}

fn scalar(fixture: &ImportRepo, sql: &str) -> i64 {
    let text = scalar_text(fixture, sql);
    text.trim()
        .parse()
        .unwrap_or_else(|_| panic!("scalar {sql}: {text}"))
}

fn scalar_text(fixture: &ImportRepo, sql: &str) -> String {
    let db = fixture.repo.join(".libra/libra.db");
    let mut command = std::process::Command::new("python3");
    let output = command
        .arg("-c")
        .arg("import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])")
        .arg(&db)
        .arg(sql)
        .output()
        .expect("query sqlite");
    assert!(output.status.success(), "{}", describe(&output));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// ---- PI-02: restricted Pi provider-root discovery ----

/// A Pi provider root (`<home>/.pi`) under an isolated `LIBRA_TEST_HOME`,
/// with a default-layout sessions root at `<home>/.pi/agent/sessions`.
struct PiDiscoveryHome {
    home: tempfile::TempDir,
    _home_env: libra::utils::test::ScopedEnvVar,
}

impl PiDiscoveryHome {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("pi home");
        let home_env = libra::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        Self {
            home,
            _home_env: home_env,
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.path().join(".pi").join("agent").join("sessions")
    }

    /// Write one session file under `--<cwd>--` with a header line and a body.
    fn write_session(&self, cwd_dir: &str, name: &str) -> PathBuf {
        let dir = self.sessions().join(cwd_dir);
        std::fs::create_dir_all(&dir).expect("pi session dir");
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{name}\",\"cwd\":\"/work\"}}\n{{\"type\":\"message\"}}\n"
            ),
        )
        .expect("pi session file");
        path
    }
}

fn pi_header(name: &str) -> Vec<u8> {
    format!("{{\"type\":\"session\",\"version\":3,\"id\":\"{name}\",\"cwd\":\"/work\"}}")
        .into_bytes()
}

/// PI-02 AC1: a relative root, or one with a `..` component, is refused
/// before anything is read.
#[test]
#[serial_test::serial(env)]
fn pi_discovery_refuses_relative_and_parent_roots() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiRootRefusal, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    home.write_session("--work--", "a.jsonl");
    for (root, reason) in [
        (
            PathBuf::from(".pi/agent/sessions"),
            PiRootRefusal::NotAbsolute,
        ),
        (PathBuf::from("sessions"), PiRootRefusal::NotAbsolute),
        (
            home.sessions().join("--work--").join("..").join("--work--"),
            PiRootRefusal::ParentComponent,
        ),
        (
            home.home.path().join(".pi").join("..").join(".pi"),
            PiRootRefusal::ParentComponent,
        ),
    ] {
        let discovery = discover_pi_sessions(
            &root,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert_eq!(discovery.refused, Some(reason), "{}", root.display());
        assert_eq!(discovery.source_reads, 0, "{}", root.display());
        assert_eq!(discovery.bytes_read, 0);
        assert_eq!(discovery.entries_scanned, 0);
        assert!(discovery.candidates.is_empty());
    }
    // Control: the same tree from its absolute root is discovered.
    let found = discover_pi_sessions(
        &home.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    assert_eq!(found.refused, None);
    assert_eq!(found.candidates.len(), 1);
    assert_eq!(found.candidates[0].header, pi_header("a.jsonl"));
}

/// PI-02 AC2: roots outside the Pi provider root are refused with zero
/// source reads, including paths taken from a session context and the
/// `sessionDir` a project's `.pi/settings.json` names (Pi reads that file
/// before trust is decided, so it grants nothing).
#[test]
#[serial_test::serial(env)]
fn pi_discovery_reads_nothing_outside_the_provider_root() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiRootRefusal, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    let project = tempfile::tempdir().expect("project");
    // A project-local session directory, named by the project's settings.
    let project_sessions = project.path().join("pi-sessions");
    std::fs::create_dir_all(project_sessions.join("--work--")).expect("project sessions");
    std::fs::write(
        project_sessions.join("--work--").join("p.jsonl"),
        "{\"type\":\"session\"}\n",
    )
    .expect("project session");
    std::fs::write(
        project_sessions.join("flat.jsonl"),
        "{\"type\":\"session\"}\n",
    )
    .expect("flat project session");
    std::fs::create_dir_all(project.path().join(".pi")).expect("project .pi");
    std::fs::write(
        project.path().join(".pi").join("settings.json"),
        format!("{{\"sessionDir\":\"{}\"}}", project_sessions.display()),
    )
    .expect("project settings");
    // Session-context paths: the working directory, a transcript path in it,
    // and the project's own `.pi` directory.
    let ctx_transcript = project.path().join("transcript.jsonl");
    std::fs::write(&ctx_transcript, "{\"type\":\"session\"}\n").expect("ctx transcript");
    let outside = tempfile::tempdir().expect("outside");
    for (root, layout) in [
        (project_sessions.clone(), PiSessionLayout::Default),
        (project_sessions.clone(), PiSessionLayout::Flat),
        (project.path().to_path_buf(), PiSessionLayout::Flat),
        (ctx_transcript.clone(), PiSessionLayout::Flat),
        (project.path().join(".pi"), PiSessionLayout::Flat),
        (outside.path().to_path_buf(), PiSessionLayout::Default),
        // The home itself is not the provider root.
        (home.home.path().to_path_buf(), PiSessionLayout::Default),
    ] {
        let discovery =
            discover_pi_sessions(&root, layout, PiDiscoveryLimits::default()).expect("discovery");
        assert_eq!(
            discovery.refused,
            Some(PiRootRefusal::OutsideProviderRoot),
            "{}",
            root.display()
        );
        assert_eq!(discovery.source_reads, 0, "{}", root.display());
        assert_eq!(discovery.bytes_read, 0, "{}", root.display());
        assert_eq!(discovery.entries_scanned, 0, "{}", root.display());
        assert!(discovery.candidates.is_empty());
    }
}

/// PI-02 AC3: a symlinked root, or a symlink between it and the provider
/// root, refuses the walk; a symlinked entry is refused without being
/// followed or read.
#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn pi_discovery_refuses_symlinks() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiPartialReason, PiRootRefusal, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    home.write_session("--work--", "real.jsonl");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    std::fs::create_dir_all(elsewhere.path().join("--other--")).expect("elsewhere dir");
    std::fs::write(
        elsewhere.path().join("--other--").join("target.jsonl"),
        "{\"type\":\"session\"}\n",
    )
    .expect("symlink target");
    let agent = home.home.path().join(".pi").join("agent");
    // The root itself, and a directory above it, as symlinks.
    std::os::unix::fs::symlink(elsewhere.path(), agent.join("linked-sessions"))
        .expect("root symlink");
    std::os::unix::fs::symlink(&agent, home.home.path().join(".pi").join("agent-link"))
        .expect("ancestor symlink");
    for root in [
        agent.join("linked-sessions"),
        home.home
            .path()
            .join(".pi")
            .join("agent-link")
            .join("sessions"),
    ] {
        let discovery = discover_pi_sessions(
            &root,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert_eq!(
            discovery.refused,
            Some(PiRootRefusal::Symlink),
            "{}",
            root.display()
        );
        assert_eq!(discovery.source_reads, 0);
        assert!(discovery.candidates.is_empty());
    }
    // Entries: a symlinked cwd directory and a symlinked session file.
    std::os::unix::fs::symlink(
        elsewhere.path().join("--other--"),
        home.sessions().join("--linked--"),
    )
    .expect("dir entry symlink");
    std::os::unix::fs::symlink(
        elsewhere.path().join("--other--").join("target.jsonl"),
        home.sessions().join("--work--").join("linked.jsonl"),
    )
    .expect("file entry symlink");
    let discovery = discover_pi_sessions(
        &home.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    assert_eq!(discovery.refused, None);
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::SymlinkEntry)
    );
    assert_eq!(discovery.source_reads, 1, "only the real file was read");
    assert_eq!(
        discovery
            .candidates
            .iter()
            .map(|candidate| candidate.path.file_name().expect("name").to_owned())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("real.jsonl")]
    );
}

/// PI-02 AC4: a FIFO or other non-regular entry is refused without being
/// opened (a FIFO opened for reading would block).
#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn pi_discovery_refuses_non_regular_files() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiPartialReason, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    home.write_session("--work--", "real.jsonl");
    let fifo = home.sessions().join("--work--").join("fifo.jsonl");
    let made = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let discovery = discover_pi_sessions(
        &home.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::NonRegularEntry)
    );
    assert_eq!(discovery.source_reads, 1, "the FIFO was not opened");
    assert_eq!(discovery.candidates.len(), 1);
    assert_eq!(discovery.candidates[0].header, pi_header("real.jsonl"));

    // A directory its group or others can write is not walked: an entry
    // there could be swapped between its type check and its open.
    use std::os::unix::fs::PermissionsExt;
    let shared = home.write_session("--shared--", "shared.jsonl");
    let shared_dir = shared.parent().expect("shared dir").to_path_buf();
    std::fs::set_permissions(&shared_dir, std::fs::Permissions::from_mode(0o777))
        .expect("make the directory writable by others");
    let discovery = discover_pi_sessions(
        &home.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    std::fs::set_permissions(&shared_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore the directory mode");
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::UnsafeDirectory)
    );
    assert!(
        discovery
            .candidates
            .iter()
            .all(|candidate| candidate.path != shared),
        "nothing under the shared directory was read"
    );
    assert_eq!(discovery.source_reads, 1);
    std::fs::remove_dir_all(&shared_dir).expect("remove the shared directory");

    // macOS: an extended ACL can let another user add or delete entries
    // whatever the mode bits say, so a directory with one is not walked.
    #[cfg(target_os = "macos")]
    {
        let acl = home.write_session("--acl--", "acl.jsonl");
        let acl_dir = acl.parent().expect("acl dir").to_path_buf();
        let granted = Command::new("/bin/chmod")
            .args(["+a", "everyone allow add_file,delete_child"])
            .arg(&acl_dir)
            .status()
            .expect("run chmod +a");
        assert!(granted.success(), "chmod +a");
        let discovery = discover_pi_sessions(
            &home.sessions(),
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::UnsafeDirectory)
        );
        assert!(
            discovery
                .candidates
                .iter()
                .all(|candidate| candidate.path != acl),
            "nothing under the ACL directory was read"
        );
        assert_eq!(discovery.source_reads, 1);
    }
}

/// PI-02 AC5: the walk inspects at most `max_entries` directory entries.
#[test]
#[serial_test::serial(env)]
fn pi_discovery_bounds_entries() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiPartialReason, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    for index in 0..6 {
        home.write_session("--work--", &format!("s{index}.jsonl"));
    }
    let limits = PiDiscoveryLimits {
        max_entries: 4,
        ..PiDiscoveryLimits::default()
    };
    let discovery = discover_pi_sessions(&home.sessions(), PiSessionLayout::Default, limits)
        .expect("discovery");
    assert_eq!(discovery.entries_scanned, 4);
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::EntryLimit)
    );
    // One entry is the `--work--` directory itself; the rest are files.
    assert_eq!(discovery.candidates.len(), 3);
    assert_eq!(discovery.source_reads, 3);
}

/// PI-02 AC6: the walk reads at most `max_total_bytes`, and at most
/// `max_header_bytes` from any one file.
#[test]
#[serial_test::serial(env)]
fn pi_discovery_bounds_bytes() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiPartialReason, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    let mut file_len = 0;
    for index in 0..4 {
        let path = home.write_session("--work--", &format!("s{index}.jsonl"));
        file_len = std::fs::metadata(path).expect("session size").len();
    }
    // Reads go in chunks, so a short file is read whole. Total budget: two
    // whole files and too little of a third to reach its newline.
    let limits = PiDiscoveryLimits {
        max_total_bytes: 2 * file_len + 3,
        ..PiDiscoveryLimits::default()
    };
    let discovery = discover_pi_sessions(&home.sessions(), PiSessionLayout::Default, limits)
        .expect("discovery");
    assert!(discovery.bytes_read <= limits.max_total_bytes);
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::ByteLimit)
    );
    assert_eq!(discovery.candidates.len(), 2);
    // Per-file cap: a header longer than it drops that file, not the walk.
    let long = home.sessions().join("--work--").join("long.jsonl");
    std::fs::write(&long, format!("{}\n", "x".repeat(4096))).expect("long header");
    let limits = PiDiscoveryLimits {
        max_header_bytes: 1024,
        ..PiDiscoveryLimits::default()
    };
    let discovery = discover_pi_sessions(&home.sessions(), PiSessionLayout::Default, limits)
        .expect("discovery");
    assert!(
        discovery
            .partial_reasons
            .contains(&PiPartialReason::ByteLimit)
    );
    assert_eq!(discovery.candidates.len(), 4, "the other files are kept");
    assert!(
        discovery
            .candidates
            .iter()
            .all(|candidate| candidate.path != long)
    );
    assert!(discovery.bytes_read <= 4 * file_len + 1024);

    // A short header in a large session file reads at most one 512-byte
    // chunk of the body, so large bodies do not use up the walk's budget.
    let large = PiDiscoveryHome::new();
    let body = "{\"type\":\"message\"}\n".repeat(64 * 1024);
    for index in 0..8 {
        let dir = large.sessions().join("--work--");
        std::fs::create_dir_all(&dir).expect("large dir");
        std::fs::write(
            dir.join(format!("big{index}.jsonl")),
            format!("{{\"type\":\"session\",\"id\":\"big{index}\"}}\n{body}"),
        )
        .expect("large session");
    }
    let discovery = discover_pi_sessions(
        &large.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    assert!(!discovery.partial(), "{:?}", discovery.partial_reasons);
    assert_eq!(discovery.candidates.len(), 8);
    assert!(
        discovery.bytes_read <= 8 * 512,
        "read {} bytes for eight headers",
        discovery.bytes_read
    );
}

/// PI-02 AC7: a walk within every limit is complete; exceeding any one limit
/// reports partial.
#[test]
#[serial_test::serial(env)]
fn pi_discovery_reports_partial_past_any_limit() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiPartialReason, PiSessionLayout, discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    for index in 0..3 {
        home.write_session("--work--", &format!("s{index}.jsonl"));
    }
    let header_len = pi_header("s0.jsonl").len() as u64;
    let complete = discover_pi_sessions(
        &home.sessions(),
        PiSessionLayout::Default,
        PiDiscoveryLimits::default(),
    )
    .expect("discovery");
    assert!(!complete.partial());
    assert_eq!(complete.candidates.len(), 3);
    for (limits, reason) in [
        (
            PiDiscoveryLimits {
                max_entries: 2,
                ..PiDiscoveryLimits::default()
            },
            PiPartialReason::EntryLimit,
        ),
        (
            PiDiscoveryLimits {
                max_total_bytes: header_len,
                ..PiDiscoveryLimits::default()
            },
            PiPartialReason::ByteLimit,
        ),
        (
            PiDiscoveryLimits {
                max_header_bytes: header_len / 2,
                ..PiDiscoveryLimits::default()
            },
            PiPartialReason::ByteLimit,
        ),
    ] {
        let discovery = discover_pi_sessions(&home.sessions(), PiSessionLayout::Default, limits)
            .expect("discovery");
        assert!(discovery.partial(), "{limits:?}");
        assert!(discovery.partial_reasons.contains(&reason), "{limits:?}");
    }
    // A flat explicit directory inside the provider root walks the same way.
    let flat = home.home.path().join(".pi").join("flat");
    std::fs::create_dir_all(&flat).expect("flat dir");
    std::fs::write(flat.join("f.jsonl"), "{\"type\":\"session\"}\n").expect("flat session");
    let flat_discovery =
        discover_pi_sessions(&flat, PiSessionLayout::Flat, PiDiscoveryLimits::default())
            .expect("discovery");
    assert!(!flat_discovery.partial());
    assert_eq!(flat_discovery.candidates.len(), 1);
}

/// PI-02 AC8: discovery does not promote Pi: its registry row stays
/// unsupported, with no hooks or transcript reading, and it is not listed.
#[test]
fn pi_discovery_keeps_pi_unsupported() {
    use libra::internal::ai::observed_agents::{
        AgentSessionCtx, agent_for, is_preview, live_capture_matches_hook_installable,
        registration_for, supported_slugs,
    };

    let row = registration_for(AgentKind::Pi);
    assert!(!row.supported);
    assert_eq!(row.support_wave, None);
    assert!(!row.transcript_readable);
    assert!(!row.hook_installable);
    assert!(!row.launchable_review && !row.launchable_investigate);
    assert!(!row.capabilities.hooks);
    assert!(!supported_slugs().contains(&"pi"));
    assert!(!is_preview(AgentKind::Pi));
    assert!(live_capture_matches_hook_installable(AgentKind::Pi));
    let agent = agent_for(AgentKind::Pi);
    assert!(agent.as_hooks().is_none());
    assert!(
        agent
            .read_transcript(&AgentSessionCtx {
                session_id: "s".to_string(),
                provider_session_id: "p".to_string(),
                working_dir: PathBuf::from("/work"),
                transcript_path: None,
            })
            .is_err()
    );
}

// ---- PI-15: Pi session identity and repository attribution ----

/// A repository directory for attribution, outside the Pi home.
struct PiAttributionRepo {
    dir: tempfile::TempDir,
}

impl PiAttributionRepo {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("pi attribution repo"),
        }
    }

    /// The root as given; on macOS a temporary directory is reached through
    /// the `/var` symlink, so this differs from the canonical root.
    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The canonical root, the form Pi records as a header `cwd`.
    fn canonical(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("canonical repo root")
    }
}

/// Pi's default-layout directory name for `cwd` (`session-manager.ts:589-594`).
fn pi_encoded_cwd_dir(cwd: &Path) -> String {
    let cwd = cwd.to_str().expect("utf-8 cwd");
    let trimmed = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    format!("--{}--", trimmed.replace(['/', '\\', ':'], "-"))
}

/// A Pi session header line, as Pi writes it.
fn pi_session_line(id: &str, cwd: &Path) -> String {
    json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": cwd.to_str().expect("utf-8 cwd"),
    })
    .to_string()
}

/// Write a session file whose first line is `first_line`; returns its
/// canonical path.
fn write_pi_file(dir: &Path, name: &str, first_line: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("pi session dir");
    let path = dir.join(name);
    std::fs::write(&path, format!("{first_line}\n{{\"type\":\"message\"}}\n"))
        .expect("pi session file");
    // Discovery reports the path with the macOS `/var` alias resolved.
    path.canonicalize().expect("canonical session path")
}

fn pi_repo_sessions(
    root: &Path,
    layout: libra::internal::ai::observed_agents::PiSessionLayout,
    repo: &Path,
) -> libra::internal::ai::observed_agents::builtin::pi::PiRepoSessions {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, builtin::pi::attribute_pi_sessions, discover_pi_sessions,
    };

    let discovery =
        discover_pi_sessions(root, layout, PiDiscoveryLimits::default()).expect("discovery");
    attribute_pi_sessions(discovery, repo).expect("attribution")
}

/// PI-15 AC1: the session id is the header `id`, never a guess from the file
/// name; a `--session-id` id need not be a UUID. A header id outside Pi's id
/// grammar, or one two files share, is not returned and makes the result
/// partial.
#[test]
#[serial_test::serial(env)]
fn pi_attribution_takes_the_session_id_from_the_header() {
    use libra::internal::ai::observed_agents::{PiSessionLayout, builtin::pi::PiSkipReason};

    let home = PiDiscoveryHome::new();
    let repo = PiAttributionRepo::new();
    let dir = home.sessions().join(pi_encoded_cwd_dir(&repo.canonical()));
    let uuid_named = write_pi_file(
        &dir,
        "2026-10-31T00-00-00-000Z_019a3c2e-7b10-7000-8000-00000000000a.jsonl",
        &pi_session_line("my-custom.session_1", &repo.canonical()),
    );
    let plain_named = write_pi_file(
        &dir,
        "notes.jsonl",
        &pi_session_line("019a3c2e-7b10-7000-8000-00000000000b", &repo.canonical()),
    );
    let only = pi_repo_sessions(&home.sessions(), PiSessionLayout::Default, repo.root());
    assert!(!only.partial(), "{only:?}");
    assert!(only.skipped.is_empty(), "{only:?}");
    assert_eq!(
        only.sessions
            .iter()
            .map(|session| (session.path.clone(), session.session_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (uuid_named.clone(), "my-custom.session_1"),
            (plain_named.clone(), "019a3c2e-7b10-7000-8000-00000000000b"),
        ]
    );
    assert!(
        only.sessions
            .iter()
            .all(|session| session.cwd == repo.canonical())
    );

    for (name, id) in [
        ("2026-10-31T00-00-01-000Z_lead.jsonl", "-lead"),
        ("2026-10-31T00-00-02-000Z_trail.jsonl", "trail."),
        ("2026-10-31T00-00-03-000Z_slash.jsonl", "a/b"),
        ("2026-10-31T00-00-04-000Z_empty.jsonl", ""),
        ("2026-10-31T00-00-05-000Z_unicode.jsonl", "café"),
    ] {
        write_pi_file(&dir, name, &pi_session_line(id, &repo.canonical()));
    }
    write_pi_file(
        &dir,
        "first-copy.jsonl",
        &pi_session_line("shared-id", &repo.canonical()),
    );
    write_pi_file(
        &dir,
        "second-copy.jsonl",
        &pi_session_line("shared-id", &repo.canonical()),
    );
    let mixed = pi_repo_sessions(&home.sessions(), PiSessionLayout::Default, repo.root());
    assert!(mixed.partial(), "{mixed:?}");
    assert_eq!(
        mixed.skipped,
        BTreeMap::from([
            (PiSkipReason::InvalidSessionId, 5),
            (PiSkipReason::DuplicateSessionId, 2),
        ])
    );
    assert_eq!(
        mixed
            .sessions
            .iter()
            .map(|session| session.path.clone())
            .collect::<Vec<_>>(),
        vec![uuid_named, plain_named]
    );
}

/// PI-15 AC2: in the default layout only sessions whose header `cwd` is the
/// repository or beneath it are returned, whichever `--<encoded-cwd>--`
/// directory holds them; a sibling whose path merely starts with the same
/// text is another directory.
#[test]
#[serial_test::serial(env)]
fn pi_attribution_default_layout_keeps_only_repo_sessions() {
    use libra::internal::ai::observed_agents::{PiSessionLayout, builtin::pi::PiSkipReason};

    let home = PiDiscoveryHome::new();
    let repo = PiAttributionRepo::new();
    let canonical = repo.canonical();
    let sub = canonical.join("crates").join("core");
    let sibling = PathBuf::from(format!("{}-sibling", canonical.display()));
    let elsewhere = tempfile::tempdir().expect("another project");
    let elsewhere = elsewhere.path().canonicalize().expect("canonical project");
    let repo_dir = home.sessions().join(pi_encoded_cwd_dir(&canonical));
    let at_root = write_pi_file(
        &repo_dir,
        "a.jsonl",
        &pi_session_line("at-root", &canonical),
    );
    write_pi_file(
        &repo_dir,
        "b.jsonl",
        &pi_session_line("moved-in", &elsewhere),
    );
    let beneath = write_pi_file(
        &home.sessions().join(pi_encoded_cwd_dir(&sub)),
        "c.jsonl",
        &pi_session_line("beneath", &sub),
    );
    write_pi_file(
        &home.sessions().join(pi_encoded_cwd_dir(&sibling)),
        "d.jsonl",
        &pi_session_line("sibling", &sibling),
    );
    write_pi_file(
        &home.sessions().join(pi_encoded_cwd_dir(&elsewhere)),
        "e.jsonl",
        &pi_session_line("elsewhere", &elsewhere),
    );
    let moved_out = write_pi_file(
        &home.sessions().join(pi_encoded_cwd_dir(&elsewhere)),
        "f.jsonl",
        &pi_session_line("moved-out", &canonical),
    );
    // A cwd recorded through the root as given (not canonical) still places
    // the session in the repository.
    let as_given = write_pi_file(
        &home.sessions().join("--as-given--"),
        "g.jsonl",
        &pi_session_line("as-given", &repo.root().join("docs")),
    );

    let found = pi_repo_sessions(&home.sessions(), PiSessionLayout::Default, repo.root());
    assert!(!found.partial(), "{found:?}");
    assert_eq!(found.refused, None);
    assert_eq!(
        found.skipped,
        BTreeMap::from([(PiSkipReason::OutsideRepo, 3)])
    );
    let mut expected = vec![
        (at_root, "at-root".to_string(), canonical.clone()),
        (beneath, "beneath".to_string(), sub),
        (moved_out, "moved-out".to_string(), canonical),
        (as_given, "as-given".to_string(), repo.root().join("docs")),
    ];
    expected.sort();
    assert_eq!(
        found
            .sessions
            .iter()
            .map(|session| (
                session.path.clone(),
                session.session_id.clone(),
                session.cwd.clone()
            ))
            .collect::<Vec<_>>(),
        expected
    );
}

/// PI-15 AC3: in an explicit (flat) session directory, which holds sessions
/// of any cwd side by side, only the repository's sessions are returned.
#[test]
#[serial_test::serial(env)]
fn pi_attribution_flat_layout_keeps_only_repo_sessions() {
    use libra::internal::ai::observed_agents::{PiSessionLayout, builtin::pi::PiSkipReason};

    let home = PiDiscoveryHome::new();
    let repo = PiAttributionRepo::new();
    let canonical = repo.canonical();
    let elsewhere = tempfile::tempdir().expect("another project");
    let elsewhere = elsewhere.path().canonicalize().expect("canonical project");
    let flat = home.home.path().join(".pi").join("shared-sessions");
    let mine = write_pi_file(&flat, "1_mine.jsonl", &pi_session_line("mine", &canonical));
    let deep = write_pi_file(
        &flat,
        "2_deep.jsonl",
        &pi_session_line("deep", &canonical.join("a").join("b")),
    );
    write_pi_file(
        &flat,
        "3_theirs.jsonl",
        &pi_session_line("theirs", &elsewhere),
    );
    write_pi_file(
        &flat,
        "4_sibling.jsonl",
        &pi_session_line(
            "sibling",
            &PathBuf::from(format!("{}-sibling", canonical.display())),
        ),
    );
    write_pi_file(
        &flat,
        "5_parent.jsonl",
        &pi_session_line("parent", canonical.parent().expect("repo parent")),
    );
    // The flat layout does not descend into a `--<encoded-cwd>--` directory.
    write_pi_file(
        &flat.join(pi_encoded_cwd_dir(&canonical)),
        "nested.jsonl",
        &pi_session_line("nested", &canonical),
    );

    let found = pi_repo_sessions(&flat, PiSessionLayout::Flat, repo.root());
    assert!(!found.partial(), "{found:?}");
    assert_eq!(
        found.skipped,
        BTreeMap::from([(PiSkipReason::OutsideRepo, 3)])
    );
    assert_eq!(
        found
            .sessions
            .iter()
            .map(|session| (session.path.clone(), session.session_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(mine, "mine"), (deep, "deep")]
    );
}

/// PI-15 AC1–AC3 boundaries: a header that cannot be placed is skipped and,
/// unless Pi itself would not list the file, makes the result partial; the
/// walk's refusal and partial reasons carry through; the repository root
/// must be absolute.
#[test]
#[serial_test::serial(env)]
fn pi_attribution_reports_what_it_cannot_place() {
    use libra::internal::ai::observed_agents::{
        PiDiscoveryLimits, PiRootRefusal, PiSessionLayout,
        builtin::pi::{PiSkipReason, attribute_pi_sessions},
        discover_pi_sessions,
    };

    let home = PiDiscoveryHome::new();
    let repo = PiAttributionRepo::new();
    let canonical = repo.canonical();
    let flat = home.home.path().join(".pi").join("odd-sessions");
    write_pi_file(
        &flat,
        "not-a-session.jsonl",
        r#"{"type":"message","id":"m1"}"#,
    );
    write_pi_file(&flat, "no-id.jsonl", r#"{"type":"session","cwd":"/work"}"#);
    let not_sessions = pi_repo_sessions(&flat, PiSessionLayout::Flat, repo.root());
    assert!(!not_sessions.partial(), "{not_sessions:?}");
    assert_eq!(
        not_sessions.skipped,
        BTreeMap::from([(PiSkipReason::NotASession, 2)])
    );

    write_pi_file(&flat, "blank.jsonl", "");
    write_pi_file(&flat, "torn.jsonl", r#"{"type":"session","id":"t"#);
    write_pi_file(&flat, "array.jsonl", r#"["session"]"#);
    write_pi_file(
        &flat,
        "repeated.jsonl",
        &format!(
            r#"{{"type":"session","id":"r1","id":"r2","cwd":{}}}"#,
            json!(canonical.to_str().expect("utf-8 cwd"))
        ),
    );
    for (name, cwd) in [
        ("missing-cwd.jsonl", None),
        ("empty-cwd.jsonl", Some(json!(""))),
        ("number-cwd.jsonl", Some(json!(7))),
        ("relative-cwd.jsonl", Some(json!("work/repo"))),
        (
            "parent-cwd.jsonl",
            Some(json!(
                canonical
                    .join("sub")
                    .join("..")
                    .to_str()
                    .expect("utf-8 cwd")
            )),
        ),
        (
            "dot-cwd.jsonl",
            Some(json!(format!(
                "{}{}.{}sub",
                canonical.display(),
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            ))),
        ),
    ] {
        let mut header =
            json!({"type": "session", "version": 3, "id": name.trim_end_matches(".jsonl")});
        if let Some(cwd) = cwd {
            header["cwd"] = cwd;
        }
        write_pi_file(&flat, name, &header.to_string());
    }
    let odd = pi_repo_sessions(&flat, PiSessionLayout::Flat, repo.root());
    assert!(odd.sessions.is_empty(), "{odd:?}");
    assert!(odd.partial(), "{odd:?}");
    assert!(odd.partial_reasons.is_empty(), "the walk itself was whole");
    assert_eq!(
        odd.skipped,
        BTreeMap::from([
            (PiSkipReason::NotASession, 2),
            (PiSkipReason::UnreadableHeader, 4),
            (PiSkipReason::NoCwd, 6),
        ])
    );

    // The walk's refusal and partial reasons carry through.
    let refused = attribute_pi_sessions(
        discover_pi_sessions(
            &repo.root().join("sessions"),
            PiSessionLayout::Flat,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery"),
        repo.root(),
    )
    .expect("attribution");
    assert_eq!(refused.refused, Some(PiRootRefusal::OutsideProviderRoot));
    assert!(refused.sessions.is_empty());
    assert!(refused.partial(), "a refused root was never scanned");
    let bounded = attribute_pi_sessions(
        discover_pi_sessions(
            &flat,
            PiSessionLayout::Flat,
            PiDiscoveryLimits {
                max_entries: 1,
                ..PiDiscoveryLimits::default()
            },
        )
        .expect("discovery"),
        repo.root(),
    )
    .expect("attribution");
    assert!(bounded.partial(), "{bounded:?}");
    assert!(!bounded.partial_reasons.is_empty());

    for relative in [PathBuf::from("repo"), canonical.join("..").join("repo")] {
        let error = attribute_pi_sessions(
            discover_pi_sessions(&flat, PiSessionLayout::Flat, PiDiscoveryLimits::default())
                .expect("discovery"),
            &relative,
        )
        .expect_err("a relative repository root is refused");
        assert!(
            error.to_string().contains("must be an absolute path"),
            "{error:#}"
        );
    }
}

// ---- PI-03: Pi v3 tree, disk leaf, and active-branch turns ----

/// A synthetic Pi v3 session file. Entry shapes follow the pinned source
/// (`session-manager.ts:43-194`, Pi `v1.4.0` `4a08025`); no real session
/// is read.
struct PiV3File {
    lines: Vec<String>,
}

impl PiV3File {
    fn new() -> Self {
        Self::with_header(json!({
            "type": "session",
            "version": 3,
            "id": "019a3c2e-7b10-7000-8000-0000000000aa",
            "timestamp": "2026-10-31T00:00:00.000Z",
            "cwd": "/work",
        }))
    }

    fn with_header(header: Value) -> Self {
        Self {
            lines: vec![header.to_string()],
        }
    }

    fn raw(mut self, line: &str) -> Self {
        self.lines.push(line.to_string());
        self
    }

    /// Append an entry of `kind` with `id` and `parent`, plus `extra` fields.
    fn entry(mut self, kind: &str, id: &str, parent: Option<&str>, extra: Value) -> Self {
        let mut entry = json!({
            "type": kind,
            "id": id,
            "parentId": parent,
            "timestamp": "2026-10-31T00:00:01.000Z",
        });
        if let (Some(entry), Value::Object(extra)) = (entry.as_object_mut(), extra) {
            entry.extend(extra);
        }
        self.lines.push(entry.to_string());
        self
    }

    fn message(self, id: &str, parent: Option<&str>, role: &str) -> Self {
        self.entry(
            "message",
            id,
            parent,
            json!({"message": {"role": role, "content": [{"type": "text", "text": "hidden"}]}}),
        )
    }

    fn bytes(&self) -> Vec<u8> {
        format!("{}\n", self.lines.join("\n")).into_bytes()
    }
}

fn pi_normalize(
    file: &PiV3File,
) -> Result<
    libra::internal::ai::observed_agents::builtin::pi::PiActiveBranch,
    libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal,
> {
    libra::internal::ai::observed_agents::builtin::pi::normalize_pi_session(
        &file.bytes(),
        Default::default(),
    )
}

fn pi_branch_ids(
    branch: &libra::internal::ai::observed_agents::builtin::pi::PiActiveBranch,
) -> Vec<&str> {
    branch
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect()
}

/// PI-03 AC1: only a header with `version` 3 is read; v1 (no version), v2,
/// a non-integer, and a newer version are refused, as is a file whose
/// first parsed line is not a session header.
#[test]
fn pi_v3_reads_only_version_3() {
    use libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal;

    let header = |version: Option<Value>| {
        let mut header = json!({"type": "session", "id": "s1", "cwd": "/work"});
        if let Some(version) = version {
            header["version"] = version;
        }
        PiV3File::with_header(header).message("u1", None, "user")
    };
    let read = pi_normalize(&header(Some(json!(3)))).expect("v3 is read");
    assert_eq!(read.session_id, "s1");
    assert_eq!(pi_branch_ids(&read), vec!["u1"]);
    for version in [
        None,
        Some(json!(1)),
        Some(json!(2)),
        Some(json!(4)),
        Some(json!("3")),
        Some(json!(3.5)),
        Some(Value::Null),
    ] {
        assert_eq!(
            pi_normalize(&header(version.clone())),
            Err(PiNormalizeRefusal::UnsupportedVersion),
            "{version:?}"
        );
    }
    for first in [
        json!({"type": "message", "id": "m1", "parentId": null}),
        json!({"type": "session", "version": 3}),
        json!(["session"]),
    ] {
        assert_eq!(
            pi_normalize(&PiV3File::with_header(first.clone()).message("u1", None, "user")),
            Err(PiNormalizeRefusal::NotASession),
            "{first}"
        );
    }
    assert_eq!(
        libra::internal::ai::observed_agents::builtin::pi::normalize_pi_session(
            b"\n\n",
            Default::default()
        ),
        Err(PiNormalizeRefusal::NotASession)
    );
    // Pi skips an unparseable line before the header; so does the reader,
    // and the result is partial.
    let mut torn_first = PiV3File::new().message("u1", None, "user");
    torn_first.lines.insert(0, "{\"type\":\"sess".to_string());
    torn_first.lines.insert(0, "null".to_string());
    let read = pi_normalize(&torn_first).expect("header after a torn line");
    assert!(read.partial());
    assert_eq!(pi_branch_ids(&read), vec!["u1"]);
    // A header line with repeated keys keeps each key's last value, as
    // `JSON.parse` does, and is partial; a last `version` that is not the
    // integer 3 is refused.
    let repeated = PiV3File {
        lines: vec![
            r#"{"type":"session","version":2,"version":3,"id":"old","id":"s9","cwd":"/work"}"#
                .to_string(),
        ],
    }
    .message("u1", None, "user");
    let read = pi_normalize(&repeated).expect("repeated-key header");
    assert_eq!(read.session_id, "s9");
    assert_eq!(
        read.partial_reasons,
        BTreeSet::from([
            libra::internal::ai::observed_agents::builtin::pi::PiBranchPartialReason::DuplicateKey
        ])
    );
    let last_version_text = PiV3File {
        lines: vec![
            r#"{"type":"session","version":3,"version":"3","id":"s1","cwd":"/work"}"#.to_string(),
        ],
    }
    .message("u1", None, "user");
    assert_eq!(
        pi_normalize(&last_version_text),
        Err(PiNormalizeRefusal::UnsupportedVersion)
    );
}

/// PI-03 AC2: every entry type in the PI-20 frozen table and every message
/// role in the PI-21 table reads whole; a type or role outside them (or
/// none), a line Pi's loader skips, a repeated key, or a second header makes
/// the result partial.
#[test]
fn pi_v3_marks_unlisted_entry_types_partial() {
    use libra::internal::ai::observed_agents::builtin::pi::PiBranchPartialReason;

    let listed = PiV3File::new()
        .message("sys", None, "system")
        .entry(
            "model_change",
            "mc",
            Some("sys"),
            json!({"provider": "p", "modelId": "m"}),
        )
        .entry(
            "thinking_level_change",
            "tl",
            Some("mc"),
            json!({"thinkingLevel": "high"}),
        )
        .message("u1", Some("tl"), "user")
        .message("a1", Some("u1"), "assistant")
        .message("t1", Some("a1"), "toolResult")
        .message("b1", Some("t1"), "bashExecution")
        .entry(
            "custom_message",
            "cm",
            Some("b1"),
            json!({"customType": "x", "content": "c", "display": true}),
        )
        .entry(
            "custom",
            "cu",
            Some("cm"),
            json!({"customType": "x", "data": {}}),
        )
        .entry(
            "usage",
            "us",
            Some("cu"),
            json!({"kind": "anything", "provider": "p", "model": "m", "usage": {}}),
        )
        .entry(
            "context_edit",
            "ce",
            Some("us"),
            json!({"targetId": "a1", "replacement": null}),
        )
        .entry(
            "branch_summary",
            "bs",
            Some("ce"),
            json!({"fromId": "u1", "summary": "s"}),
        )
        .entry(
            "label",
            "lb",
            Some("bs"),
            json!({"targetId": "u1", "label": "l"}),
        )
        .entry("session_info", "si", Some("lb"), json!({"name": "n"}))
        .entry(
            "compaction",
            "co",
            Some("si"),
            json!({"summary": "s", "firstKeptEntryId": "u1", "tokensBefore": 1}),
        );
    let whole = pi_normalize(&listed).expect("listed types");
    assert!(!whole.partial(), "{:?}", whole.partial_reasons);
    assert_eq!(whole.entries.len(), 15);

    for (file, reason) in [
        (
            PiV3File::new().message("u1", None, "user").entry(
                "future_entry",
                "f1",
                Some("u1"),
                json!({}),
            ),
            PiBranchPartialReason::UnknownEntryType,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .message("c1", Some("u1"), "custom"),
            PiBranchPartialReason::UnknownMessageRole,
        ),
        (
            PiV3File::new().message("u1", None, "user").entry(
                "message",
                "c2",
                Some("u1"),
                json!({"message": {}}),
            ),
            PiBranchPartialReason::UnknownMessageRole,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .raw("{\"type\":\"message\",\"id\":\"torn\""),
            PiBranchPartialReason::MalformedLine,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .raw(r#"{"id":"x1","parentId":"u1"}"#),
            PiBranchPartialReason::UnknownEntryType,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .raw(r#"{"type":"label","id":"l1","parentId":"u1","label":"a","label":"b"}"#),
            PiBranchPartialReason::DuplicateKey,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .raw("null")
                .raw("0")
                .raw("false")
                .raw("\"\""),
            PiBranchPartialReason::MalformedLine,
        ),
        (
            PiV3File::new()
                .message("u1", None, "user")
                .raw(r#"{"type":"session","version":3,"id":"again","cwd":"/work"}"#),
            PiBranchPartialReason::ExtraHeader,
        ),
    ] {
        let branch = pi_normalize(&file).expect("partial, not refused");
        assert_eq!(
            branch.partial_reasons,
            BTreeSet::from([reason]),
            "{:?}",
            file.lines
        );
    }
}

/// PI-03 AC3: the disk leaf is Pi's: the last entry `_buildIndex` indexes
/// in file order (`session-manager.ts:1103-1121`) after the loader drops
/// lines `JSON.parse` rejects and falsy values (`:616-648`), and the branch
/// is the walk from it to its root (`getBranch`, `:1469-1479`). An
/// independent oracle reads each fixture with that algorithm, including a
/// trailing torn line, an entry without a `type`, and a repeated key. An
/// entry whose place in Pi's tree cannot be reproduced refuses the file
/// instead of yielding an earlier leaf.
#[test]
fn pi_v3_recovers_the_disk_leaf() {
    use libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal;

    /// The pinned recovery algorithm (`JSON.parse` keeps a repeated key's
    /// last value, as `serde_json::Value` does).
    fn oracle(file: &PiV3File) -> (Option<String>, Vec<String>) {
        let entries: Vec<Value> = file.lines[1..]
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| {
                !matches!(entry, Value::Null | Value::Bool(false))
                    && entry.as_f64() != Some(0.0)
                    && entry.as_str() != Some("")
            })
            .filter(|entry| entry["type"] != "session")
            .collect();
        let leaf = entries
            .last()
            .map(|entry| entry["id"].as_str().expect("id").to_string());
        let mut path = Vec::new();
        let mut current = leaf.clone();
        while let Some(id) = current {
            let entry = entries
                .iter()
                .find(|entry| entry["id"] == id.as_str())
                .expect("entry");
            path.push(id);
            current = entry["parentId"].as_str().map(str::to_string);
        }
        path.reverse();
        (leaf, path)
    }

    let base = || {
        PiV3File::new()
            .message("u1", None, "user")
            .message("a1", Some("u1"), "assistant")
            .message("u2", Some("a1"), "user")
            .message("a2", Some("u2"), "assistant")
    };
    let fixtures = [
        base(),
        // A branch back to `a1`, appended after the abandoned `u2`/`a2`.
        base()
            .message("u3", Some("a1"), "user")
            .message("a3", Some("u3"), "assistant"),
        // A label appended last becomes the leaf.
        base().entry(
            "label",
            "l1",
            Some("a2"),
            json!({"targetId": "u1", "label": "x"}),
        ),
        // A new root appended last leaves the old tree off the branch.
        base().message("r1", None, "user"),
        // Only a header.
        PiV3File::new(),
        // A torn last line is dropped by the loader: the leaf is before it.
        base().raw(r#"{"type":"message","id":"torn","parentId":"a2""#),
        // An entry without a `type` is still indexed and is the leaf.
        base().raw(r#"{"id":"untyped","parentId":"a1"}"#),
        // A repeated key keeps its last value, so the leaf is `last`, a
        // child of `u2`; falsy lines are dropped.
        base()
            .raw("null")
            .raw(r#"{"type":"label","id":"first","id":"last","parentId":"a2","parentId":"u2"}"#),
        // A second header is not indexed.
        base().raw(r#"{"type":"session","version":3,"id":"again","cwd":"/work"}"#),
    ];
    for file in &fixtures {
        let branch = pi_normalize(file).expect("normalize");
        let (leaf, path) = oracle(file);
        assert_eq!(branch.leaf_id, leaf, "{:?}", file.lines);
        assert_eq!(
            branch
                .entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            path,
            "{:?}",
            file.lines
        );
    }
    assert_eq!(
        pi_normalize(&fixtures[6])
            .expect("normalize")
            .leaf_id
            .as_deref(),
        Some("untyped")
    );
    assert_eq!(
        pi_branch_ids(&pi_normalize(&fixtures[7]).expect("normalize")),
        vec!["u1", "a1", "u2", "last"]
    );
    let branched = pi_normalize(&fixtures[1]).expect("normalize");
    assert_eq!(pi_branch_ids(&branched), vec!["u1", "a1", "u3", "a3"]);
    assert_eq!(branched.off_branch_entries, 2);
    // Lines point back at each entry's line in the file (header is line 1).
    assert_eq!(
        branched
            .entries
            .iter()
            .map(|entry| entry.line)
            .collect::<Vec<_>>(),
        vec![2, 3, 6, 7]
    );

    for line in [
        r#"{"type":"label","parentId":"a2"}"#,
        r#"{"type":"label","id":7,"parentId":"a2"}"#,
        r#"{"type":"label","id":"","parentId":"a2"}"#,
        r#"{"type":"label","id":"x","parentId":7}"#,
        r#"{"type":"label","id":"x","parentId":""}"#,
        r#"{"type":"label","id":"x","parentId":false}"#,
        "5",
        "\"text\"",
        "[1]",
        "true",
    ] {
        assert_eq!(
            pi_normalize(&base().raw(line)),
            Err(PiNormalizeRefusal::UnrepresentableEntry),
            "{line}"
        );
    }
}

/// PI-03 AC4: two entries with one `id` refuse the file, on or off the
/// branch, including an `id` a repeated key sets.
#[test]
fn pi_v3_refuses_duplicate_entry_ids() {
    use libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal;

    for file in [
        PiV3File::new()
            .message("u1", None, "user")
            .message("u1", Some("u1"), "assistant"),
        PiV3File::new()
            .message("u1", None, "user")
            .message("x", Some("u1"), "assistant")
            .message("x", Some("u1"), "assistant")
            .message("u2", Some("u1"), "user"),
        // A repeated `id` key whose last value repeats another entry's id.
        PiV3File::new()
            .message("u1", None, "user")
            .raw(r#"{"type":"label","id":"fresh","id":"u1","parentId":null}"#),
    ] {
        assert_eq!(
            pi_normalize(&file),
            Err(PiNormalizeRefusal::DuplicateEntryId),
            "{:?}",
            file.lines
        );
    }
}

/// PI-03 AC5: a `parentId` that names no entry refuses the file, on or off
/// the branch, including one that names a skipped malformed line.
#[test]
fn pi_v3_refuses_a_missing_parent() {
    use libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal;

    for file in [
        PiV3File::new()
            .message("u1", None, "user")
            .message("a1", Some("ghost"), "assistant"),
        PiV3File::new()
            .message("u1", None, "user")
            .message("x", Some("ghost"), "assistant")
            .message("a1", Some("u1"), "assistant"),
        PiV3File::new()
            .message("u1", None, "user")
            .raw(r#"{"type":"message","id":"torn","parentId":"u1""#)
            .message("a1", Some("torn"), "assistant"),
    ] {
        assert_eq!(
            pi_normalize(&file),
            Err(PiNormalizeRefusal::MissingParent),
            "{:?}",
            file.lines
        );
    }
}

/// PI-03 AC6: a parent cycle refuses the file, whether it is reachable from
/// the leaf, a self-parent, or off the branch.
#[test]
fn pi_v3_refuses_a_parent_cycle() {
    use libra::internal::ai::observed_agents::builtin::pi::PiNormalizeRefusal;

    for file in [
        PiV3File::new()
            .message("a", Some("b"), "user")
            .message("b", Some("a"), "assistant"),
        PiV3File::new().message("self", Some("self"), "user"),
        PiV3File::new()
            .message("x", Some("z"), "user")
            .message("y", Some("x"), "assistant")
            .message("z", Some("y"), "user")
            .message("u1", None, "user"),
    ] {
        assert_eq!(
            pi_normalize(&file),
            Err(PiNormalizeRefusal::Cycle),
            "{:?}",
            file.lines
        );
    }
}

/// PI-03 AC7: turns before the latest compaction boundary are incomplete.
/// A retain-none compaction (`firstKeptEntryId` is its own id) puts the
/// boundary at itself; a kept entry earlier on the branch puts it there; a
/// kept entry that is not earlier on the branch is partial and falls back
/// to the compaction. Only the latest compaction on the branch counts.
#[test]
fn pi_v3_marks_history_before_compaction_incomplete() {
    use libra::internal::ai::observed_agents::{
        builtin::pi::PiBranchPartialReason, coverage::Completeness,
    };

    let with_compaction = |kept: &str| {
        PiV3File::new()
            .message("u1", None, "user")
            .message("a1", Some("u1"), "assistant")
            .message("u2", Some("a1"), "user")
            .message("a2", Some("u2"), "assistant")
            .entry(
                "compaction",
                "c1",
                Some("a2"),
                json!({"summary": "s", "firstKeptEntryId": kept, "tokensBefore": 10}),
            )
            .message("u3", Some("c1"), "user")
            .message("a3", Some("u3"), "assistant")
    };
    let completeness =
        |branch: &libra::internal::ai::observed_agents::builtin::pi::PiActiveBranch| {
            branch
                .turns
                .iter()
                .map(|turn| turn.completeness)
                .collect::<Vec<_>>()
        };

    let retain_none = pi_normalize(&with_compaction("c1")).expect("retain-none");
    assert!(!retain_none.partial());
    assert_eq!(retain_none.before_compaction, 0..4);
    assert_eq!(
        completeness(&retain_none),
        vec![
            Completeness::Incomplete,
            Completeness::Incomplete,
            Completeness::Complete
        ]
    );

    let keeps_u2 = pi_normalize(&with_compaction("u2")).expect("keeps u2");
    assert!(!keeps_u2.partial());
    assert_eq!(keeps_u2.before_compaction, 0..2);
    assert_eq!(
        completeness(&keeps_u2),
        vec![
            Completeness::Incomplete,
            Completeness::Complete,
            Completeness::Complete
        ]
    );

    let dangling = pi_normalize(&with_compaction("elsewhere")).expect("dangling kept id");
    assert_eq!(
        dangling.partial_reasons,
        BTreeSet::from([PiBranchPartialReason::CompactionBoundaryNotOnBranch])
    );
    assert_eq!(dangling.before_compaction, 0..4);

    // Only the latest compaction on the branch sets the boundary, as in
    // `buildContextEntries`: a later compaction may keep history an earlier
    // retain-none compaction summarized.
    let twice = |first_kept: &str, second_kept: &str| {
        with_compaction(first_kept)
            .entry(
                "compaction",
                "c2",
                Some("a3"),
                json!({"summary": "s", "firstKeptEntryId": second_kept, "tokensBefore": 20}),
            )
            .message("u4", Some("c2"), "user")
            .message("a4", Some("u4"), "assistant")
    };
    use Completeness::{Complete, Incomplete};
    for (first_kept, second_kept, before, expected, partial) in [
        (
            "c1",
            "u2",
            0..2,
            vec![Incomplete, Complete, Complete, Complete],
            false,
        ),
        (
            "u1",
            "u3",
            0..5,
            vec![Incomplete, Incomplete, Complete, Complete],
            false,
        ),
        (
            "c1",
            "c2",
            0..7,
            vec![Incomplete, Incomplete, Incomplete, Complete],
            false,
        ),
        (
            "u2",
            "u4",
            0..7,
            vec![Incomplete, Incomplete, Incomplete, Complete],
            true,
        ),
    ] {
        let branch = pi_normalize(&twice(first_kept, second_kept)).expect("two compactions");
        assert_eq!(
            branch.before_compaction, before,
            "{first_kept} {second_kept}"
        );
        assert_eq!(
            completeness(&branch),
            expected,
            "{first_kept} {second_kept}"
        );
        assert_eq!(branch.partial(), partial, "{first_kept} {second_kept}");
    }

    // A compaction on an abandoned branch does not touch the active one.
    let off_branch = PiV3File::new()
        .message("u1", None, "user")
        .message("a1", Some("u1"), "assistant")
        .entry(
            "compaction",
            "c1",
            Some("a1"),
            json!({"summary": "s", "firstKeptEntryId": "c1", "tokensBefore": 10}),
        )
        .message("u2", Some("a1"), "user");
    let off_branch = pi_normalize(&off_branch).expect("off-branch compaction");
    assert_eq!(off_branch.before_compaction, 0..0);
    assert!(
        completeness(&off_branch)
            .iter()
            .all(|completeness| *completeness == Completeness::Complete)
    );
}

/// PI-03 AC8: only a `user` role message opens a turn. A system message,
/// model change, bash execution, custom message, usage entry, and unknown
/// type open none; entries before the first user message are a preamble.
#[test]
fn pi_v3_only_user_messages_open_turns() {
    let file = PiV3File::new()
        .message("sys", None, "system")
        .entry(
            "model_change",
            "mc",
            Some("sys"),
            json!({"provider": "p", "modelId": "m"}),
        )
        .message("u1", Some("mc"), "user")
        .message("a1", Some("u1"), "assistant")
        .message("t1", Some("a1"), "toolResult")
        .message("b1", Some("t1"), "bashExecution")
        .entry(
            "custom_message",
            "cm",
            Some("b1"),
            json!({"customType": "x", "content": "c", "display": true}),
        )
        .entry(
            "usage",
            "us",
            Some("cm"),
            json!({"kind": "k", "provider": "p", "model": "m", "usage": {}}),
        )
        .entry("future_entry", "fu", Some("us"), json!({}))
        .message("sys2", Some("fu"), "system")
        .message("u2", Some("sys2"), "user")
        .message("a2", Some("u2"), "assistant");
    let branch = pi_normalize(&file).expect("normalize");
    assert!(branch.partial(), "the unknown type is partial");
    assert_eq!(branch.preamble, 0..2);
    assert_eq!(
        branch
            .turns
            .iter()
            .map(|turn| (
                turn.ordinal,
                turn.opening_entry_id.as_str(),
                turn.entries.clone()
            ))
            .collect::<Vec<_>>(),
        vec![(0, "u1", 2..10), (1, "u2", 10..12)]
    );

    let no_user =
        PiV3File::new()
            .message("sys", None, "system")
            .message("a1", Some("sys"), "assistant");
    let no_user = pi_normalize(&no_user).expect("normalize");
    assert!(no_user.turns.is_empty());
    assert_eq!(no_user.preamble, 0..2);
}

// ---- PI-11: Pi v3 message content projection ----

fn pi_project(
    file: &PiV3File,
) -> libra::internal::ai::observed_agents::builtin::pi::PiContentProjection {
    let bytes = file.bytes();
    let branch = libra::internal::ai::observed_agents::builtin::pi::normalize_pi_session(
        &bytes,
        Default::default(),
    )
    .expect("normalize");
    libra::internal::ai::observed_agents::builtin::pi::project_pi_content(&bytes, &branch)
        .expect("project")
}

/// The records of every turn and the preamble, in branch order.
fn pi_records(
    projection: &libra::internal::ai::observed_agents::builtin::pi::PiContentProjection,
) -> Vec<libra::internal::ai::observed_agents::builtin::pi::PiContentRecord> {
    projection
        .preamble
        .iter()
        .chain(projection.turns.iter().flat_map(|turn| turn.records.iter()))
        .cloned()
        .collect()
}

/// An assistant `thinking` block written with odd spacing, key order, an
/// unknown field, and a signature.
const PI_THINKING_BLOCK: &str = r#"{ "thinkingSignature":"sig==" ,"type":"thinking","thinking":"weigh the options","extra":{"n":1.50,"k":[true,null]},"redacted":false }"#;

/// PI-11 AC1: a thinking block is kept as its exact JSON text, every field
/// included, readable or redacted; projecting other bytes than the branch
/// was normalized from fails.
#[test]
fn pi_content_projection_keeps_thinking_blocks_whole() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PiContentRecord, PiProjectionError, normalize_pi_session, project_pi_content,
    };

    let redacted =
        r#"{"type":"thinking","thinking":"","thinkingSignature":"opaque-payload","redacted":true}"#;
    let file = PiV3File::new()
        .message("u1", None, "user")
        .raw(&format!(
            r#"{{"type":"message","id":"a1","parentId":"u1","message":{{"role":"assistant","content":[{PI_THINKING_BLOCK},{redacted},{{"type":"text","text":"done"}}]}}}}"#
        ));
    let projection = pi_project(&file);
    let blocks: Vec<String> = pi_records(&projection)
        .into_iter()
        .filter_map(|record| match record {
            PiContentRecord::Reasoning { block, .. } => Some(block.get().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        blocks,
        vec![PI_THINKING_BLOCK.to_string(), redacted.to_string()]
    );
    assert!(!projection.partial(), "{:?}", projection.partial_reasons);

    // Under a repeated `message` or `content` key the last value is kept,
    // still byte for byte.
    let repeated = PiV3File::new().message("u1", None, "user").raw(&format!(
        r#"{{"type":"message","id":"a1","parentId":"u1","message":{{"role":"assistant","content":[{{"type":"text","text":"stale"}}]}},"message":{{"role":"assistant","content":[{{"type":"text","text":"also stale"}}],"content":[{PI_THINKING_BLOCK}]}}}}"#
    ));
    let records = pi_records(&pi_project(&repeated));
    assert_eq!(
        records
            .iter()
            .filter_map(|record| match record {
                PiContentRecord::Reasoning { block, .. } => Some(block.get().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![PI_THINKING_BLOCK.to_string()]
    );
    assert!(!format!("{records:?}").contains("stale"));

    let bytes = file.bytes();
    let branch = normalize_pi_session(&bytes, Default::default()).expect("normalize");
    let other = PiV3File::new()
        .message("x1", None, "user")
        .message("x2", Some("x1"), "assistant")
        .bytes();
    assert_eq!(
        project_pi_content(&other, &branch),
        Err(PiProjectionError::BranchMismatch)
    );
    assert_eq!(
        project_pi_content(b"", &branch),
        Err(PiProjectionError::BranchMismatch)
    );
}

/// PI-11 AC2: a toolCall block's `thoughtSignature` is kept exactly as
/// written, in the field and in the whole block.
#[test]
fn pi_content_projection_keeps_tool_call_thought_signature() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentRecord;

    let call = r#"{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"a.rs"},"thoughtSignature": {"v" :2,"blob":"Q0FGRQ=="} }"#;
    let plain = r#"{"type":"toolCall","id":"call-2","name":"ls","arguments":{}}"#;
    let repeated = r#"{"type":"toolCall","id":"call-3","name":"ls","arguments":{},"thoughtSignature":"first","thoughtSignature": [ "last",1.0 ] }"#;
    let file = PiV3File::new().message("u1", None, "user").raw(&format!(
        r#"{{"type":"message","id":"a1","parentId":"u1","message":{{"role":"assistant","content":[{call},{plain},{repeated}]}}}}"#
    ));
    let calls: Vec<_> = pi_records(&pi_project(&file))
        .into_iter()
        .filter_map(|record| match record {
            PiContentRecord::ToolCall {
                call_id,
                name,
                arguments,
                thought_signature,
                block,
            } => Some((
                call_id,
                name,
                arguments,
                thought_signature.map(|signature| signature.get().to_string()),
                block.get().to_string(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            (
                Some("call-1".to_string()),
                Some("read".to_string()),
                Some(json!({"path": "a.rs"})),
                Some(r#"{"v" :2,"blob":"Q0FGRQ=="}"#.to_string()),
                call.to_string(),
            ),
            (
                Some("call-2".to_string()),
                Some("ls".to_string()),
                Some(json!({})),
                None,
                plain.to_string(),
            ),
            (
                Some("call-3".to_string()),
                Some("ls".to_string()),
                Some(json!({})),
                Some(r#"[ "last",1.0 ]"#.to_string()),
                repeated.to_string(),
            ),
        ]
    );
}

/// PI-11 AC3: readable thinking (a non-empty `thinking` that is not
/// `redacted`) maps to a reasoning record; an empty or redacted block keeps
/// its block with no readable text. The text never shows in `Debug`.
#[test]
fn pi_content_projection_maps_readable_thinking_to_reasoning() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentRecord;

    let file = PiV3File::new().message("u1", None, "user").raw(&format!(
        r#"{{"type":"message","id":"a1","parentId":"u1","message":{{"role":"assistant","content":[{PI_THINKING_BLOCK},{{"type":"thinking","thinking":""}},{{"type":"thinking","thinking":"hidden","redacted":true}}]}}}}"#
    ));
    let projection = pi_project(&file);
    let readable: Vec<Option<String>> = pi_records(&projection)
        .into_iter()
        .filter_map(|record| match record {
            PiContentRecord::Reasoning { readable, .. } => {
                Some(readable.map(|text| text.as_str().to_string()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        readable,
        vec![Some("weigh the options".to_string()), None, None]
    );
    let debug = format!("{projection:?}");
    assert!(!debug.contains("weigh the options"), "{debug}");
    assert!(!debug.contains("sig=="), "{debug}");
    assert!(!projection.partial(), "{:?}", projection.partial_reasons);

    // An ill-typed redaction state or thinking text is not readable and is
    // partial; the block is still kept.
    for block in [
        r#"{"type":"thinking","thinking":"leak","redacted":"true"}"#,
        r#"{"type":"thinking","thinking":"leak","redacted":1}"#,
        r#"{"type":"thinking","thinking":"leak","redacted":null}"#,
        r#"{"type":"thinking"}"#,
        r#"{"type":"thinking","thinking":7}"#,
    ] {
        let file = PiV3File::new().message("u1", None, "user").raw(&format!(
            r#"{{"type":"message","id":"a1","parentId":"u1","message":{{"role":"assistant","content":[{block}]}}}}"#
        ));
        let projection = pi_project(&file);
        assert_eq!(
            projection.partial_reasons,
            BTreeSet::from([
                libra::internal::ai::observed_agents::builtin::pi::PiContentPartialReason::MalformedField
            ]),
            "{block}"
        );
        assert_eq!(
            pi_records(&projection)
                .into_iter()
                .filter_map(|record| match record {
                    PiContentRecord::Reasoning { readable, block } =>
                        Some((readable.is_some(), block.get().to_string())),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![(false, block.to_string())],
            "{block}"
        );
    }
}

/// PI-11 AC4: readable thinking never joins the assistant answer: the
/// answer records hold only `text` blocks.
#[test]
fn pi_content_projection_keeps_thinking_out_of_the_answer() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentRecord;

    let file = PiV3File::new().message("u1", None, "user").raw(
        r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret plan"},{"type":"text","text":"the answer"},{"type":"thinking","thinking":"second thought"},{"type":"text","text":"more"}]}}"#,
    );
    let records = pi_records(&pi_project(&file));
    let answers: Vec<&str> = records
        .iter()
        .filter_map(|record| match record {
            PiContentRecord::Assistant { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec!["the answer", "more"]);
    let reasoning: Vec<&str> = records
        .iter()
        .filter_map(|record| match record {
            PiContentRecord::Reasoning {
                readable: Some(text),
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, vec!["secret plan", "second thought"]);
}

/// PI-11 AC5: a `context_edit` changes only Pi's later model context: the
/// target's recorded text is projected unchanged, whether the edit
/// replaces it or omits it (`replacement: null`).
#[test]
fn pi_content_projection_does_not_apply_context_edit() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentRecord;

    let file = PiV3File::new()
        .entry(
            "message",
            "u1",
            None,
            json!({"message": {"role": "user", "content": "original question"}}),
        )
        .entry(
            "message",
            "a1",
            Some("u1"),
            json!({"message": {"role": "assistant", "content": [{"type": "text", "text": "original answer"}]}}),
        )
        .entry(
            "context_edit",
            "e1",
            Some("a1"),
            json!({"targetId": "a1", "replacement": {"content": "rewritten answer"}}),
        )
        .entry(
            "context_edit",
            "e2",
            Some("e1"),
            json!({"targetId": "u1", "replacement": null}),
        )
        .message("u2", Some("e2"), "user");
    let records = pi_records(&pi_project(&file));
    assert!(records.contains(&PiContentRecord::User {
        text: "original question".to_string()
    }));
    assert!(records.contains(&PiContentRecord::Assistant {
        text: "original answer".to_string()
    }));
    assert!(!format!("{records:?}").contains("rewritten answer"));
}

/// PI-11 AC6: a tool result whose `toolCallId` matches no tool call
/// earlier on the branch is partial: an unknown id, no id, and a call that
/// only an abandoned branch made.
#[test]
fn pi_content_projection_marks_orphan_tool_results_partial() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentPartialReason;

    let call = |id: &str, parent: Option<&str>, call: &str| {
        (
            id.to_string(),
            parent.map(str::to_string),
            json!({"message": {"role": "assistant", "content": [{"type": "toolCall", "id": call, "name": "read", "arguments": {}}]}}),
        )
    };
    let result = |call: Option<&str>| {
        let mut message = json!({"role": "toolResult", "toolName": "read", "content": [{"type": "text", "text": "ok"}], "isError": false});
        if let Some(call) = call {
            message["toolCallId"] = json!(call);
        }
        json!({ "message": message })
    };
    let (id, parent, body) = call("a1", Some("u1"), "call-1");
    let matched = PiV3File::new()
        .message("u1", None, "user")
        .entry("message", &id, parent.as_deref(), body)
        .entry("message", "r1", Some("a1"), result(Some("call-1")));
    assert!(!pi_project(&matched).partial());

    for orphan in [
        PiV3File::new().message("u1", None, "user").entry(
            "message",
            "r1",
            Some("u1"),
            result(Some("call-9")),
        ),
        PiV3File::new().message("u1", None, "user").entry(
            "message",
            "r1",
            Some("u1"),
            result(None),
        ),
        {
            let (id, parent, body) = call("a1", Some("u1"), "call-1");
            PiV3File::new()
                .message("u1", None, "user")
                .entry("message", &id, parent.as_deref(), body)
                .message("u2", Some("u1"), "user")
                .entry("message", "r1", Some("u2"), result(Some("call-1")))
        },
    ] {
        assert_eq!(
            pi_project(&orphan).partial_reasons,
            BTreeSet::from([PiContentPartialReason::OrphanToolResult]),
            "{:?}",
            orphan.lines
        );
    }

    // A `null` `details` is kept as written.
    let mut body = result(Some("call-1"));
    body["message"]["details"] = Value::Null;
    let (id, parent, call_body) = call("a1", Some("u1"), "call-1");
    let with_null_details = PiV3File::new()
        .message("u1", None, "user")
        .entry("message", &id, parent.as_deref(), call_body)
        .entry("message", "r1", Some("a1"), body);
    let projection = pi_project(&with_null_details);
    assert!(!projection.partial(), "{:?}", projection.partial_reasons);
    assert!(pi_records(&projection).iter().any(|record| matches!(
        record,
        libra::internal::ai::observed_agents::builtin::pi::PiContentRecord::ToolResult {
            details: Some(details),
            ..
        } if details.get() == "null"
    )));

    // A missing or ill-typed `isError` is unknown, not success.
    for is_error in [None, Some(json!("yes"))] {
        let mut body = result(Some("call-1"));
        match &is_error {
            Some(value) => body["message"]["isError"] = value.clone(),
            None => {
                if let Some(message) = body["message"].as_object_mut() {
                    message.remove("isError");
                }
            }
        }
        let (id, parent, call_body) = call("a1", Some("u1"), "call-1");
        let file = PiV3File::new()
            .message("u1", None, "user")
            .entry("message", &id, parent.as_deref(), call_body)
            .entry("message", "r1", Some("a1"), body);
        let projection = pi_project(&file);
        assert_eq!(
            projection.partial_reasons,
            BTreeSet::from([PiContentPartialReason::MalformedField]),
            "{is_error:?}"
        );
        assert!(pi_records(&projection).iter().any(|record| matches!(
            record,
            libra::internal::ai::observed_agents::builtin::pi::PiContentRecord::ToolResult {
                is_error: None,
                ..
            }
        )));
    }
}

/// PI-11 AC7: `nestedCalls` stays bounded metadata of its tool result: it
/// adds no tool call or result of its own; past the call or byte bound only
/// its count is kept.
#[test]
fn pi_content_projection_keeps_nested_calls_as_bounded_metadata() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PI_NESTED_CALLS_MAX_BYTES, PI_NESTED_CALLS_MAX_CALLS, PiContentPartialReason,
        PiContentRecord,
    };

    let with_nested = |nested: &str| {
        PiV3File::new()
            .message("u1", None, "user")
            .entry(
                "message",
                "a1",
                Some("u1"),
                json!({"message": {"role": "assistant", "content": [{"type": "toolCall", "id": "call-1", "name": "codemode", "arguments": {}}]}}),
            )
            .raw(&format!(
                r#"{{"type":"message","id":"r1","parentId":"a1","message":{{"role":"toolResult","toolCallId":"call-1","toolName":"codemode","content":[{{"type":"text","text":"ran"}}],"isError":false,"nestedCalls":{nested}}}}}"#
            ))
    };
    let nested = r#"{"calls":[{"id":"n1","name":"read","status":"ok","arguments":{"path":"x"},"durationMs":3},{"id":"n2","name":"bash","status":"error","error":"boom"}],"complete":true}"#;
    let projection = pi_project(&with_nested(nested));
    assert!(!projection.partial(), "{:?}", projection.partial_reasons);
    let records = pi_records(&projection);
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, PiContentRecord::ToolCall { .. }))
            .count(),
        1,
        "nested calls are not tool calls of the turn"
    );
    let results: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            PiContentRecord::ToolResult {
                call_id,
                text,
                nested_calls,
                ..
            } => Some((call_id.clone(), text.clone(), nested_calls.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    let (call_id, text, nested_calls) = &results[0];
    assert_eq!(call_id.as_deref(), Some("call-1"));
    assert_eq!(text, &vec!["ran".to_string()]);
    let nested_calls = nested_calls.as_ref().expect("nested calls");
    assert_eq!(nested_calls.calls, 2);
    assert_eq!(nested_calls.complete, Some(true));
    assert_eq!(nested_calls.raw.as_ref().map(|raw| raw.get()), Some(nested));

    let many = format!(
        r#"{{"calls":[{}],"complete":true}}"#,
        vec![r#"{"id":"n","name":"read","status":"ok"}"#; PI_NESTED_CALLS_MAX_CALLS + 1].join(",")
    );
    let large = format!(
        r#"{{"calls":[{{"id":"n","name":"read","status":"error","error":"{}"}}],"complete":true}}"#,
        "e".repeat(PI_NESTED_CALLS_MAX_BYTES)
    );
    // Near the session byte bound: a million short calls are counted
    // without being kept.
    let huge = format!(
        r#"{{"calls":[{}],"complete":true}}"#,
        vec!["0"; 1_000_000].join(",")
    );
    for (nested, count) in [
        (many.as_str(), PI_NESTED_CALLS_MAX_CALLS + 1),
        (large.as_str(), 1),
        (huge.as_str(), 1_000_000),
        (r#"{"complete":true}"#, 0),
    ] {
        let projection = pi_project(&with_nested(nested));
        assert_eq!(
            projection.partial_reasons,
            BTreeSet::from([PiContentPartialReason::NestedCallsOverBound])
        );
        let kept = pi_records(&projection)
            .into_iter()
            .find_map(|record| match record {
                PiContentRecord::ToolResult { nested_calls, .. } => nested_calls,
                _ => None,
            })
            .expect("nested calls");
        assert_eq!(kept.calls, count);
        assert_eq!(kept.raw, None);
    }
}

/// PI-11 AC8: `nestedCalls.complete: false` (or a `complete` that is not a
/// boolean) is partial.
#[test]
fn pi_content_projection_marks_incomplete_nested_calls_partial() {
    use libra::internal::ai::observed_agents::builtin::pi::PiContentPartialReason;

    for nested in [
        r#"{"calls":[{"id":"n1","name":"read","status":"unfinished"}],"complete":false}"#,
        r#"{"calls":[{"id":"n1","name":"read","status":"ok","argumentsBytes":9000}],"complete":false}"#,
        r#"{"calls":[]}"#,
        r#"{"calls":[],"complete":"yes"}"#,
    ] {
        let file = PiV3File::new()
            .message("u1", None, "user")
            .entry(
                "message",
                "a1",
                Some("u1"),
                json!({"message": {"role": "assistant", "content": [{"type": "toolCall", "id": "c1", "name": "codemode", "arguments": {}}]}}),
            )
            .raw(&format!(
                r#"{{"type":"message","id":"r1","parentId":"a1","message":{{"role":"toolResult","toolCallId":"c1","toolName":"codemode","content":[],"isError":false,"nestedCalls":{nested}}}}}"#
            ));
        assert_eq!(
            pi_project(&file).partial_reasons,
            BTreeSet::from([PiContentPartialReason::NestedCallsIncomplete]),
            "{nested}"
        );
    }
}

/// PI-11 scope: readable entries that open no turn are projected as
/// context records with their source kept as written: system content and
/// sections (in JavaScript enumeration order, removed ones skipped), a bash
/// execution's command and output, an extension message, branch and
/// compaction summaries, and a compaction's `systemMessage`. Records keep
/// block order, and a large `sections` object is read in linear time.
#[test]
fn pi_content_projection_keeps_readable_context_entries() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PiContentPartialReason, PiContentRecord, PiContextKind,
    };

    let system = r#"{"type":"message","id":"s1","parentId":null,"message":{"role":"system","content":"base prompt","sections":{"zeta":"first section","alpha":null,"mid":"second section"},"toolsAdded":[{"name":"read"}]}}"#;
    let file = PiV3File::new()
        .raw(system)
        .message("u1", Some("s1"), "user")
        .entry(
            "message",
            "b1",
            Some("u1"),
            json!({"message": {"role": "bashExecution", "command": "ls", "output": "a.rs", "exitCode": 0, "cancelled": false, "truncated": false}}),
        )
        .entry(
            "custom_message",
            "c1",
            Some("b1"),
            json!({"customType": "note", "content": [{"type": "text", "text": "from an extension"}], "display": false}),
        )
        .entry(
            "branch_summary",
            "bs",
            Some("c1"),
            json!({"fromId": "u1", "summary": "tried another way"}),
        )
        .raw(r#"{"type":"compaction","id":"co","parentId":"bs","summary":"earlier work","firstKeptEntryId":"co","tokensBefore":5,"systemMessage":{"role":"system","content":"checkpoint prompt","sections":{"10":"ten","b":"bee","2":"two","a":null,"b":"bee again"}}}"#);
    let projection = pi_project(&file);
    assert!(!projection.partial(), "{:?}", projection.partial_reasons);
    let contexts: Vec<(PiContextKind, Vec<String>)> = pi_records(&projection)
        .into_iter()
        .filter_map(|record| match record {
            PiContentRecord::Context { kind, text, .. } => Some((kind, text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        contexts,
        vec![
            (
                PiContextKind::System,
                vec![
                    "base prompt".to_string(),
                    "first section".to_string(),
                    "second section".to_string()
                ]
            ),
            (
                PiContextKind::BashExecution,
                vec!["ls".to_string(), "a.rs".to_string()]
            ),
            (
                PiContextKind::CustomMessage,
                vec!["from an extension".to_string()]
            ),
            (
                PiContextKind::BranchSummary,
                vec!["tried another way".to_string()]
            ),
            // The compaction's `systemMessage` comes before its summary, its
            // sections in JavaScript order: array-index names ascending,
            // then the rest in file order, a repeated name keeping its first
            // position and last value.
            (
                PiContextKind::System,
                vec![
                    "checkpoint prompt".to_string(),
                    "two".to_string(),
                    "ten".to_string(),
                    "bee again".to_string()
                ]
            ),
            (
                PiContextKind::CompactionSummary,
                vec!["earlier work".to_string()]
            ),
        ]
    );
    // The system message sits in the preamble and keeps its whole entry.
    assert!(matches!(
        projection.preamble.as_slice(),
        [PiContentRecord::Context { kind: PiContextKind::System, entry, .. }] if entry.get() == system
    ));

    let broken = PiV3File::new()
        .message("u1", None, "user")
        .entry(
            "message",
            "b1",
            Some("u1"),
            json!({"message": {"role": "bashExecution", "command": 3}}),
        )
        .entry("branch_summary", "bs", Some("b1"), json!({"fromId": "u1"}))
        .entry(
            "compaction",
            "co",
            Some("bs"),
            json!({"summary": "s", "firstKeptEntryId": "co", "tokensBefore": 1, "systemMessage": "not an object"}),
        );
    assert_eq!(
        pi_project(&broken).partial_reasons,
        BTreeSet::from([PiContentPartialReason::MalformedField])
    );

    // A `content` of a JSON type the message does not allow is kept whole.
    let ill_typed = PiV3File::new()
        .raw(r#"{"type":"message","id":"u1","parentId":null,"message":{"role":"user","content":7}}"#)
        .raw(r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":"plain answer"}}"#)
        .raw(r#"{"type":"message","id":"a2","parentId":"a1","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"read","arguments":{}}]}}"#)
        .raw(r#"{"type":"message","id":"r1","parentId":"a2","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":{"x" : 1},"isError":false}}"#);
    let projection = pi_project(&ill_typed);
    assert_eq!(
        projection.partial_reasons,
        BTreeSet::from([PiContentPartialReason::UnknownBlock])
    );
    assert_eq!(
        pi_records(&projection)
            .into_iter()
            .filter_map(|record| match record {
                PiContentRecord::Unprojected { block } => Some(block.get().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![
            "7".to_string(),
            r#""plain answer""#.to_string(),
            r#"{"x" : 1}"#.to_string()
        ]
    );

    // User text and a kept image block stay in block order.
    let interleaved = PiV3File::new().raw(
        r#"{"type":"message","id":"u1","parentId":null,"message":{"role":"user","content":[{"type":"text","text":"before"},{"type":"image","data":"AAAA","mimeType":"image/png"},{"type":"text","text":"after"}]}}"#,
    );
    let projection = pi_project(&interleaved);
    assert_eq!(
        projection.partial_reasons,
        BTreeSet::from([PiContentPartialReason::UnsupportedBlock])
    );
    assert_eq!(
        pi_records(&projection)
            .into_iter()
            .map(|record| match record {
                PiContentRecord::User { text } => text,
                PiContentRecord::Unprojected { block } => format!("kept {}", block.get().len()),
                other => panic!("unexpected record {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![
            "before".to_string(),
            format!(
                "kept {}",
                r#"{"type":"image","data":"AAAA","mimeType":"image/png"}"#.len()
            ),
            "after".to_string()
        ]
    );

    // Many distinct section names are read in one pass.
    let names = 200_000;
    let sections = (0..names)
        .map(|index| format!("\"s{index}\":\"v\""))
        .collect::<Vec<_>>()
        .join(",");
    let large = PiV3File::new().raw(&format!(
        r#"{{"type":"message","id":"s1","parentId":null,"message":{{"role":"system","content":"p","sections":{{{sections}}}}}}}"#
    ));
    let started = Instant::now();
    let projection = pi_project(&large);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(
        projection.preamble.as_slice(),
        [PiContentRecord::Context { text, .. }] if text.len() == names + 1
    ));

    // An entry that may hold content but cannot be projected is kept whole
    // and is partial: a message without a `message` object, one with an
    // unknown role, and an unknown entry type.
    let no_message = r#"{"type":"message","id":"m1","parentId":"u1","content":"stray"}"#;
    let custom_role = r#"{"type":"message","id":"m2","parentId":"m1","message":{"role":"custom","content":"hi"}}"#;
    let future = r#"{"type":"future_entry","id":"m3","parentId":"m2","text":"new"}"#;
    let file = PiV3File::new()
        .message("u1", None, "user")
        .raw(no_message)
        .raw(custom_role)
        .raw(future);
    let projection = pi_project(&file);
    assert_eq!(
        projection.partial_reasons,
        BTreeSet::from([PiContentPartialReason::UnprojectedEntry])
    );
    assert_eq!(
        pi_records(&projection)
            .into_iter()
            .filter_map(|record| match record {
                PiContentRecord::Unprojected { block } => Some(block.get().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![
            no_message.to_string(),
            custom_role.to_string(),
            future.to_string()
        ]
    );
}

// ---- PI-16: Pi session usage totals and model attribution ----

/// A Pi `Usage` value (`ai/src/types.ts`), with only `cost.total` set
/// among the cost parts.
fn pi_usage(input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) -> Value {
    json!({
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "totalTokens": input + output + cache_read + cache_write,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": cost},
    })
}

fn pi_assistant(model: &str, response_model: Option<Value>, usage: Option<Value>) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": [{"type": "text", "text": "ok"}],
        "provider": "deepseek",
        "model": model,
    });
    if let Some(response_model) = response_model {
        message["responseModel"] = response_model;
    }
    if let Some(usage) = usage {
        message["usage"] = usage;
    }
    json!({ "message": message })
}

/// Every usage source of one session: the active branch (with history
/// before a compaction), an abandoned branch, a `usage` entry of an
/// unknown kind, compaction and branch-summary usage, a tool result with
/// and without usage, and a `model_change` naming a virtual model.
fn pi_usage_session() -> PiV3File {
    PiV3File::new()
        .entry(
            "model_change",
            "mc",
            None,
            json!({"provider": "virtual", "modelId": "auto-router"}),
        )
        .message("u1", Some("mc"), "user")
        .entry(
            "message",
            "a1",
            Some("u1"),
            pi_assistant("deepseek-v4-pro", None, Some(pi_usage(10, 20, 1, 2, 0.5))),
        )
        .entry(
            "message",
            "t1",
            Some("a1"),
            json!({"message": {"role": "toolResult", "toolCallId": "c1", "toolName": "read", "content": [], "isError": false, "usage": pi_usage(1, 1, 0, 0, 0.25)}}),
        )
        .entry(
            "message",
            "t2",
            Some("t1"),
            json!({"message": {"role": "toolResult", "toolCallId": "c2", "toolName": "read", "content": [], "isError": false}}),
        )
        .entry(
            "usage",
            "us",
            Some("t2"),
            json!({"kind": "never-seen-kind", "provider": "deepseek", "model": "deepseek-flash", "usage": pi_usage(3, 4, 0, 0, 0.125)}),
        )
        .entry(
            "compaction",
            "co",
            Some("us"),
            json!({"summary": "s", "firstKeptEntryId": "co", "tokensBefore": 40, "usage": pi_usage(5, 5, 0, 0, 0.0625)}),
        )
        .entry(
            "branch_summary",
            "bs",
            Some("co"),
            json!({"fromId": "u1", "summary": "s", "usage": pi_usage(2, 2, 0, 0, 0.03125)}),
        )
        .message("u2", Some("bs"), "user")
        // Abandoned: a sibling answer to `u1` that the active branch left.
        .entry(
            "message",
            "a2",
            Some("u1"),
            pi_assistant("deepseek-v4-pro", None, Some(pi_usage(100, 100, 0, 0, 1.0))),
        )
        .entry(
            "message",
            "a3",
            Some("u2"),
            pi_assistant(
                "deepseek-v4-pro",
                Some(json!("deepseek-v4-pro-0610")),
                Some(pi_usage(0, 0, 0, 0, 0.0)),
            ),
        )
}

fn pi_metadata(
    file: &PiV3File,
) -> libra::internal::ai::observed_agents::builtin::pi::PiSessionMetadata {
    libra::internal::ai::observed_agents::builtin::pi::project_pi_session_metadata(
        &file.bytes(),
        Default::default(),
    )
    .expect("session metadata")
}

/// PI-16 AC1: the session token total is the sum of every usage source in
/// every raw entry, as Pi's `getSessionStats` counts it: off the active
/// branch, before a compaction, a `usage` entry of an unknown kind,
/// compaction and branch-summary usage, and tool-result and assistant usage.
#[test]
fn pi_session_metadata_totals_tokens_over_every_entry() {
    let file = pi_usage_session();
    let metadata = pi_metadata(&file);
    assert!(!metadata.partial(), "{:?}", metadata.partial_reasons);
    assert_eq!(metadata.usage_sources, 7);
    assert_eq!(
        (
            metadata.totals.input,
            metadata.totals.output,
            metadata.totals.cache_read,
            metadata.totals.cache_write,
        ),
        (121, 132, 1, 2)
    );
    assert_eq!(metadata.totals.tokens(), 256);
    // The abandoned branch is counted: the active branch alone is short.
    let branch = libra::internal::ai::observed_agents::builtin::pi::normalize_pi_session(
        &file.bytes(),
        Default::default(),
    )
    .expect("normalize");
    assert!(!branch.entries.iter().any(|entry| entry.id == "a2"));
}

/// PI-16 AC2: the session cost total sums `cost.total` over the same
/// sources.
#[test]
fn pi_session_metadata_totals_cost_over_every_entry() {
    let metadata = pi_metadata(&pi_usage_session());
    assert_eq!(
        metadata.totals.cost,
        0.5 + 0.25 + 0.125 + 0.0625 + 0.03125 + 1.0
    );
    assert_eq!(metadata.totals.cost, 1.96875);
}

/// PI-16 AC3: an assistant message with a `responseModel` is attributed to
/// it, even when it differs from `model`.
#[test]
fn pi_session_metadata_attributes_the_response_model() {
    let metadata = pi_metadata(&pi_usage_session());
    let a3 = metadata
        .models
        .iter()
        .find(|model| model.entry_id == "a3")
        .expect("a3");
    assert_eq!(a3.model.as_deref(), Some("deepseek-v4-pro-0610"));
    assert!(a3.from_response_model);
    assert_eq!(a3.provider.as_deref(), Some("deepseek"));
}

/// PI-16 AC4: without a `responseModel` (absent or `null`, as `??` treats
/// them) the model is the message's `model`, never a `model_change`'s
/// possibly virtual `modelId`. Ill-typed models and usage are partial and
/// left out.
#[test]
fn pi_session_metadata_falls_back_to_the_message_model() {
    use libra::internal::ai::observed_agents::builtin::pi::PiMetadataPartialReason;

    let metadata = pi_metadata(&pi_usage_session());
    assert_eq!(
        metadata
            .models
            .iter()
            .map(|model| (
                model.entry_id.as_str(),
                model.model.as_deref(),
                model.from_response_model
            ))
            .collect::<Vec<_>>(),
        vec![
            ("a1", Some("deepseek-v4-pro"), false),
            ("a2", Some("deepseek-v4-pro"), false),
            ("a3", Some("deepseek-v4-pro-0610"), true),
        ]
    );
    assert!(
        metadata
            .models
            .iter()
            .all(|model| model.model.as_deref() != Some("auto-router"))
    );

    let null_response = PiV3File::new()
        .entry(
            "model_change",
            "mc",
            None,
            json!({"provider": "virtual", "modelId": "auto-router"}),
        )
        .message("u1", Some("mc"), "user")
        .entry(
            "message",
            "a1",
            Some("u1"),
            pi_assistant(
                "deepseek-flash",
                Some(Value::Null),
                Some(pi_usage(1, 1, 0, 0, 0.0)),
            ),
        );
    let metadata = pi_metadata(&null_response);
    assert!(!metadata.partial(), "{:?}", metadata.partial_reasons);
    assert_eq!(metadata.models[0].model.as_deref(), Some("deepseek-flash"));
    assert!(!metadata.models[0].from_response_model);

    for (assistant, reason) in [
        (
            pi_assistant("m", Some(json!(7)), Some(pi_usage(1, 1, 0, 0, 0.0))),
            PiMetadataPartialReason::MalformedModel,
        ),
        (
            pi_assistant("m", None, None),
            PiMetadataPartialReason::MalformedUsage,
        ),
        (
            pi_assistant(
                "m",
                None,
                Some(
                    json!({"input": 1.5, "output": 1, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0}}),
                ),
            ),
            PiMetadataPartialReason::MalformedUsage,
        ),
        (
            pi_assistant(
                "m",
                None,
                Some(
                    json!({"input": -1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0}}),
                ),
            ),
            PiMetadataPartialReason::MalformedUsage,
        ),
        (
            pi_assistant(
                "m",
                None,
                Some(json!({"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0})),
            ),
            PiMetadataPartialReason::MalformedUsage,
        ),
        (
            {
                let mut assistant = pi_assistant("m", None, Some(pi_usage(1, 1, 0, 0, 0.0)));
                if let Some(message) = assistant["message"].as_object_mut() {
                    message.remove("provider");
                }
                assistant
            },
            PiMetadataPartialReason::MalformedModel,
        ),
    ] {
        let file = PiV3File::new().message("u1", None, "user").entry(
            "message",
            "a1",
            Some("u1"),
            assistant.clone(),
        );
        let metadata = pi_metadata(&file);
        assert_eq!(
            metadata.partial_reasons,
            BTreeSet::from([reason]),
            "{assistant}"
        );
    }
    let overflow = PiV3File::new()
        .message("u1", None, "user")
        .entry(
            "message",
            "a1",
            Some("u1"),
            pi_assistant("m", None, Some(pi_usage(u64::MAX, 0, 0, 0, 0.0))),
        )
        .entry(
            "message",
            "a2",
            Some("a1"),
            pi_assistant("m", None, Some(pi_usage(1, 0, 0, 0, 0.0))),
        );
    let metadata = pi_metadata(&overflow);
    assert_eq!(
        metadata.partial_reasons,
        BTreeSet::from([PiMetadataPartialReason::TokenOverflow])
    );
    assert_eq!(metadata.totals.input, u64::MAX);

    // One usage whose counters fit but whose four-way sum does not.
    let sum_overflow = PiV3File::new()
        .message("u1", None, "user")
        .entry("message", "a1", Some("u1"), pi_assistant("m", None, Some(pi_usage(u64::MAX, 0, 0, 0, 0.0))))
        .entry("usage", "us", Some("a1"), json!({"kind": "k", "provider": "p", "model": "m", "usage": {"input": 0, "output": 1, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0}}}));
    let metadata = pi_metadata(&sum_overflow);
    assert_eq!(
        metadata.partial_reasons,
        BTreeSet::from([PiMetadataPartialReason::TokenOverflow])
    );
    assert_eq!(metadata.totals.tokens(), u64::MAX);

    // Finite costs that sum past `f64::MAX`.
    let cost_overflow = PiV3File::new()
        .message("u1", None, "user")
        .entry(
            "message",
            "a1",
            Some("u1"),
            pi_assistant("m", None, Some(pi_usage(0, 0, 0, 0, 1e308))),
        )
        .entry(
            "message",
            "a2",
            Some("a1"),
            pi_assistant("m", None, Some(pi_usage(0, 0, 0, 0, 1e308))),
        );
    assert_eq!(
        pi_metadata(&cost_overflow).partial_reasons,
        BTreeSet::from([PiMetadataPartialReason::CostOverflow])
    );

    // A usage entry on a line the normalizer skipped (torn) may be missing
    // from the totals.
    let torn = PiV3File::new()
        .message("u1", None, "user")
        .entry(
            "message",
            "a1",
            Some("u1"),
            pi_assistant("m", None, Some(pi_usage(1, 1, 0, 0, 0.0))),
        )
        .raw(r#"{"type":"usage","id":"us","parentId":"a1","usage":{"input":5"#);
    let metadata = pi_metadata(&torn);
    assert_eq!(
        metadata.partial_reasons,
        BTreeSet::from([PiMetadataPartialReason::SkippedLine])
    );
    assert_eq!(metadata.totals.tokens(), 2);
}

// ---- PI-10: explicit Pi import ----

const PI10_SESSION_ID: &str = "019a3c2e-7b10-7000-8000-0000000000b1";

/// A Pi v3 session whose header places it in `cwd`: one prompt, and an
/// answer with thinking and its signature.
fn pi10_session(id: &str, cwd: &Path) -> Vec<u8> {
    PiV3File::with_header(json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": cwd.to_str().expect("utf-8 cwd"),
    }))
    .entry(
        "message",
        "u1",
        None,
        json!({"message": {"role": "user", "content": "pi10-prompt"}}),
    )
    .entry(
        "message",
        "a1",
        Some("u1"),
        json!({"message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "pi10-thinking", "thinkingSignature": "pi10-signature"},
            {"type": "text", "text": "pi10-answer"},
        ]}}),
    )
    .bytes()
}

/// Write `bytes` the way Pi names a session file, `<timestamp>_<id>.jsonl`,
/// under the fixture's own home (never the real `~/.pi`).
fn pi10_write(fixture: &ImportRepo, id: &str, bytes: &[u8]) -> PathBuf {
    let dir = fixture.home.join(".pi/agent/sessions/pi10");
    std::fs::create_dir_all(&dir).expect("pi session dir");
    let path = dir.join(format!("2026-10-31T00-00-00-000Z_{id}.jsonl"));
    std::fs::write(&path, bytes).expect("pi session file");
    path
}

fn pi10_import(fixture: &ImportRepo, path: &Path, extra: &[&str]) -> Output {
    let path = path_arg(path);
    let mut args = vec!["agent", "import", "--agent", "pi", "--path", path.as_str()];
    args.extend_from_slice(extra);
    fixture.run(&args)
}

/// Every blob under the checkpoint's directory in the traces tree, by path.
fn pi10_checkpoint_blobs(
    storage_root: &Path,
    root_tree_oid: &str,
    checkpoint_id: &str,
) -> Vec<(String, Vec<u8>)> {
    let mut oid = root_tree_oid.to_string();
    for component in ["checkpoint", &checkpoint_id[..2], &checkpoint_id[2..]] {
        oid = tree_entry_oid(&read_object_payload(storage_root, &oid), component);
    }
    let mut blobs = Vec::new();
    let mut pending = vec![(String::new(), oid)];
    while let Some((prefix, oid)) = pending.pop() {
        let tree = read_object_payload(storage_root, &oid);
        let mut cursor = 0usize;
        while cursor < tree.len() {
            let mode_end = cursor
                + tree[cursor..]
                    .iter()
                    .position(|byte| *byte == b' ')
                    .expect("tree entry mode delimiter");
            let name_start = mode_end + 1;
            let name_end = name_start
                + tree[name_start..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .expect("tree entry name delimiter");
            let entry_oid = hex::encode(&tree[name_end + 1..name_end + 21]);
            let name = format!(
                "{prefix}{}",
                String::from_utf8_lossy(&tree[name_start..name_end])
            );
            if &tree[cursor..mode_end] == b"40000" {
                pending.push((format!("{name}/"), entry_oid));
            } else {
                blobs.push((name, read_object_payload(storage_root, &entry_oid)));
            }
            cursor = name_end + 21;
        }
    }
    blobs.sort();
    blobs
}

fn pi10_session_rows(fixture: &ImportRepo) -> i64 {
    scalar(
        fixture,
        "SELECT COUNT(*) FROM agent_session WHERE agent_kind = 'pi'",
    )
}

fn pi10_shown(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// PI-10 AC1: without consent the import stops before it reads the source.
/// Neither a missing file nor one it may not read surfaces a source error;
/// the refusal is the consent requirement, and nothing is written.
#[test]
fn pi_explicit_import_refused_consent_reads_no_source() {
    let fixture = ImportRepo::init();
    let missing = fixture
        .home
        .join(format!("2026-10-31T00-00-00-000Z_{PI10_SESSION_ID}.jsonl"));
    let present = pi10_write(
        &fixture,
        PI10_SESSION_ID,
        &pi10_session(PI10_SESSION_ID, &fixture.repo),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&present, std::fs::Permissions::from_mode(0o000))
            .expect("make the session unreadable");
    }
    for path in [&missing, &present] {
        let output = pi10_import(&fixture, path, &["--json"]);
        assert!(!output.status.success(), "{}", describe(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{}", describe(&output));
        assert!(stderr.contains("--yes"), "{}", describe(&output));
        assert!(
            !stderr.contains("source authorization") && !stderr.contains("pi10-"),
            "the source must not be read before consent: {}",
            describe(&output)
        );
    }
    assert_eq!(pi10_session_rows(&fixture), 0);
    assert_eq!(checkpoint_count(&fixture), 0);
}

/// PI-10 AC2-AC4 and AC8: one explicit import stores the PI-33 snapshot as
/// one partial checkpoint through the shared coordinator and claim store
/// (one identity, one coverage claim); its snapshot is the Pi frame's
/// unproven partial one. A second import of the same logical turn writes
/// nothing. Output never echoes session content, and Pi stays unsupported.
#[test]
fn pi_explicit_import_stores_one_partial_checkpoint_and_replays_as_noop() {
    use libra::internal::ai::observed_agents::registration_for;

    let fixture = ImportRepo::init();
    let path = pi10_write(
        &fixture,
        PI10_SESSION_ID,
        &pi10_session(PI10_SESSION_ID, &fixture.repo),
    );
    let first = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(first.status.success(), "{}", describe(&first));
    let shown = pi10_shown(&first);
    for hidden in [
        "pi10-prompt",
        "pi10-thinking",
        "pi10-signature",
        "pi10-answer",
    ] {
        assert!(!shown.contains(hidden), "{hidden}: {shown}");
    }
    assert_eq!(checkpoint_count(&fixture), 1);
    assert_eq!(pi10_session_rows(&fixture), 1);
    assert_eq!(identity_state(&fixture), "committed");
    assert_eq!(
        scalar(&fixture, "SELECT COUNT(*) FROM agent_import_identity"),
        1
    );
    assert_eq!(
        scalar(&fixture, "SELECT COUNT(*) FROM agent_coverage_claim"),
        1
    );
    assert_eq!(
        scalar_text(&fixture, "SELECT session_id FROM agent_session"),
        format!("pi__{PI10_SESSION_ID}")
    );
    let checkpoint_id = scalar_text(&fixture, "SELECT checkpoint_id FROM agent_checkpoint");
    let tree_oid = scalar_text(&fixture, "SELECT tree_oid FROM agent_checkpoint");
    let metadata: Value = serde_json::from_slice(&read_checkpoint_blob(
        &fixture.repo.join(".libra"),
        &tree_oid,
        &checkpoint_id,
        &["metadata.json"],
    ))
    .expect("parse the checkpoint metadata");
    let snapshot = &metadata["transcript_snapshot"];
    assert_eq!(snapshot["completeness"], "partial", "{metadata}");
    assert_eq!(
        snapshot["partial_reason"], "unproven_ordinary",
        "{metadata}"
    );
    // What the checkpoint stores: ordinary turns with no reasoning record,
    // and every thinking block (text and signature as written) kept beside
    // them as retained, unproven data.
    let blobs: BTreeMap<String, Vec<u8>> =
        pi10_checkpoint_blobs(&fixture.repo.join(".libra"), &tree_oid, &checkpoint_id)
            .into_iter()
            .collect();
    let turn: Value = serde_json::from_slice(
        blobs
            .get("transcript/pi.jsonl")
            .expect("the turn projection"),
    )
    .expect("parse the turn projection");
    assert_eq!(
        turn["records"],
        json!([
            {"text": "pi10-prompt", "type": "user"},
            {"text": "pi10-answer", "type": "assistant"},
        ]),
        "{turn}"
    );
    assert_eq!(turn["completeness"], "incomplete", "{turn}");
    let report: Value = serde_json::from_slice(
        blobs
            .get("redaction_report.json")
            .expect("the redaction report"),
    )
    .expect("parse the redaction report");
    assert_eq!(report["pi_source_state"], "unproven", "{report}");
    let stored_metadata: Value =
        serde_json::from_slice(blobs.get("metadata.json").expect("the checkpoint metadata"))
            .expect("parse the checkpoint metadata");
    assert!(
        !String::from_utf8_lossy(blobs.get("metadata.json").expect("metadata"))
            .contains("pi10-thinking"),
        "metadata keeps no thinking text"
    );
    assert_eq!(
        stored_metadata["redaction_report"]["pi_retained_blocks"], 1,
        "{stored_metadata}"
    );
    assert_eq!(
        report["pi_retained"],
        json!([{
            "turn_key": "u1",
            "thinking": "pi10-thinking",
            "thinkingSignature": "pi10-signature",
        }]),
        "{report}"
    );

    let second = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(second.status.success(), "{}", describe(&second));
    assert_eq!(checkpoint_count(&fixture), 1, "a replay wrote a checkpoint");
    assert_eq!(
        scalar(&fixture, "SELECT COUNT(*) FROM agent_coverage_claim"),
        1
    );

    let registration = registration_for(AgentKind::Pi);
    assert!(!registration.supported);
    assert!(!registration.hook_installable);
    let listed = fixture.run(&["agent", "list", "--json"]);
    assert!(listed.status.success(), "{}", describe(&listed));
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("\"pi\""),
        "{}",
        describe(&listed)
    );
    let enabled = fixture.run(&["agent", "add", "pi"]);
    assert!(!enabled.status.success(), "{}", describe(&enabled));
}

/// PI-10 AC2: an import uses only the PI-33 snapshot. When its frame refuses
/// the session (here a parent cycle, a bad frame), the snapshot keeps no
/// transcript and the import stores nothing; no other path reads the file.
#[test]
fn pi_explicit_import_refused_frame_stores_nothing() {
    let fixture = ImportRepo::init();
    let bytes = PiV3File::with_header(json!({
        "type": "session",
        "version": 3,
        "id": PI10_SESSION_ID,
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": fixture.repo.to_str().expect("utf-8 cwd"),
    }))
    .entry(
        "message",
        "u1",
        Some("a1"),
        json!({"message": {"role": "user", "content": "pi10-prompt"}}),
    )
    .entry(
        "message",
        "a1",
        Some("u1"),
        json!({"message": {"role": "assistant", "content": [{"type": "text", "text": "pi10-answer"}]}}),
    )
    .bytes();
    let path = pi10_write(&fixture, PI10_SESSION_ID, &bytes);
    let objects_before = loose_object_file_count(&fixture.repo.join(".libra/objects"));
    let output = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        !pi10_shown(&output).contains("pi10-"),
        "{}",
        describe(&output)
    );
    assert_eq!(pi10_session_rows(&fixture), 0);
    assert_eq!(checkpoint_count(&fixture), 0);
    assert_eq!(
        loose_object_file_count(&fixture.repo.join(".libra/objects")),
        objects_before
    );
}

/// PI-10 AC5 and AC6: the session row commits before any trace object, index
/// row, or ref: a session write that fails stops the import before an object
/// exists. A checkpoint write that fails after the objects leaves nothing
/// committed, and the retry, through the same coordinator, completes with
/// exactly one checkpoint; a replay adds none. (The in-process crash after a
/// durable turn is `pi_explicit_import_resumes_after_an_interrupted_attempt`
/// in `agent_import.rs`.)
#[tokio::test]
async fn pi_explicit_import_commits_the_session_first_and_retries_once() {
    let fixture = ImportRepo::init();
    let path = pi10_write(
        &fixture,
        PI10_SESSION_ID,
        &pi10_session(PI10_SESSION_ID, &fixture.repo),
    );
    let db_url = format!(
        "sqlite://{}",
        fixture.repo.join(".libra/libra.db").display()
    );
    let conn = Database::connect(db_url)
        .await
        .expect("open writable repo db");
    let execute = |sql: &'static str| {
        let conn = conn.clone();
        async move {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect(sql);
        }
    };
    let objects = || loose_object_file_count(&fixture.repo.join(".libra/objects"));

    execute(
        "CREATE TRIGGER pi10_reject_session BEFORE INSERT ON agent_session
         BEGIN SELECT RAISE(ABORT, 'pi10 session write failure'); END",
    )
    .await;
    let objects_before = objects();
    let refused = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(!refused.status.success(), "{}", describe(&refused));
    assert_eq!(pi10_session_rows(&fixture), 0);
    assert_eq!(
        objects(),
        objects_before,
        "an object was written before the session committed"
    );
    assert_eq!(scalar(&fixture, "SELECT COUNT(*) FROM object_index"), 0);
    execute("DROP TRIGGER pi10_reject_session").await;

    execute(
        "CREATE TRIGGER pi10_reject_checkpoint BEFORE INSERT ON agent_checkpoint
         BEGIN SELECT RAISE(ABORT, 'pi10 checkpoint write failure'); END",
    )
    .await;
    let interrupted = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(!interrupted.status.success(), "{}", describe(&interrupted));
    assert!(
        !pi10_shown(&interrupted).contains("pi10-"),
        "{}",
        describe(&interrupted)
    );
    assert_eq!(checkpoint_count(&fixture), 0);
    execute("DROP TRIGGER pi10_reject_checkpoint").await;
    conn.close().await.expect("close repo db");

    let retried = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(retried.status.success(), "{}", describe(&retried));
    assert_eq!(checkpoint_count(&fixture), 1);
    assert_eq!(identity_state(&fixture), "committed");
    let replayed = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(replayed.status.success(), "{}", describe(&replayed));
    assert_eq!(checkpoint_count(&fixture), 1, "a retry committed twice");
}

/// PI-10 AC7: a session cannot be tied to another repository or another
/// session. A header whose `cwd` is outside the repository, or whose `id` is
/// not the file's, is refused without a write; two sessions stay two
/// sessions with their own identities.
#[test]
fn pi_explicit_import_keeps_repositories_and_sessions_apart() {
    let fixture = ImportRepo::init();
    let elsewhere = fixture._tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("other directory");
    let elsewhere = elsewhere.canonicalize().expect("canonical other directory");
    let outside = pi10_write(
        &fixture,
        PI10_SESSION_ID,
        &pi10_session(PI10_SESSION_ID, &elsewhere),
    );
    let refused = pi10_import(&fixture, &outside, &["--yes", "--json"]);
    assert!(!refused.status.success(), "{}", describe(&refused));
    assert_eq!(pi10_session_rows(&fixture), 0);

    let other_id = "019a3c2e-7b10-7000-8000-0000000000b2";
    let renamed = pi10_write(
        &fixture,
        other_id,
        &pi10_session(PI10_SESSION_ID, &fixture.repo),
    );
    let mismatched = pi10_import(&fixture, &renamed, &["--yes", "--json"]);
    assert!(!mismatched.status.success(), "{}", describe(&mismatched));
    assert_eq!(pi10_session_rows(&fixture), 0);

    for id in [PI10_SESSION_ID, other_id] {
        let path = pi10_write(&fixture, id, &pi10_session(id, &fixture.repo));
        let output = pi10_import(&fixture, &path, &["--yes", "--json"]);
        assert!(output.status.success(), "{}", describe(&output));
    }
    assert_eq!(pi10_session_rows(&fixture), 2);
    assert_eq!(checkpoint_count(&fixture), 2);
    assert_eq!(
        scalar(
            &fixture,
            "SELECT COUNT(DISTINCT identity_id) FROM agent_import_identity"
        ),
        2
    );
    assert_eq!(
        scalar(
            &fixture,
            "SELECT COUNT(DISTINCT session_id) FROM agent_checkpoint"
        ),
        2
    );

    // Another Pi profile (another agent directory) may hold a file with the
    // same custom id for the same repository. It is a different source, so
    // it is refused rather than joined to the session already stored.
    let other_profile = fixture.home.join(".pi-other/agent/sessions/pi10");
    std::fs::create_dir_all(&other_profile).expect("other profile directory");
    let foreign = other_profile.join(format!("2026-10-31T00-00-00-000Z_{PI10_SESSION_ID}.jsonl"));
    let mut body = pi10_session(PI10_SESSION_ID, &fixture.repo);
    body.extend_from_slice(
        json!({"type": "message", "id": "u9", "parentId": "a1",
               "timestamp": "2026-10-31T00:00:02.000Z",
               "message": {"role": "user", "content": "pi10-foreign"}})
        .to_string()
        .as_bytes(),
    );
    body.push(b'\n');
    std::fs::write(&foreign, body).expect("other profile session");
    let joined = pi10_import(&fixture, &foreign, &["--yes", "--json"]);
    assert!(!joined.status.success(), "{}", describe(&joined));
    assert!(
        String::from_utf8_lossy(&joined.stderr).contains("LBR-AGENT-015"),
        "the refusal is the session ownership conflict: {}",
        describe(&joined)
    );
    assert!(
        !pi10_shown(&joined).contains("pi10-"),
        "{}",
        describe(&joined)
    );
    assert_eq!(pi10_session_rows(&fixture), 2);
    assert_eq!(
        checkpoint_count(&fixture),
        2,
        "the other profile's turn was joined"
    );
}

/// PI-10 AC8: `agent import` accepts `--agent pi` only with `--path`, its
/// help names Pi, and the refusals for a scan or a session id say why.
#[test]
fn pi_explicit_import_requires_one_path_and_is_named_in_help() {
    let fixture = ImportRepo::init();
    let help = fixture.run(&["agent", "import", "--help"]);
    assert!(help.status.success(), "{}", describe(&help));
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("`hermes`, or `pi`"), "{help}");
    assert!(help.contains("--agent pi"), "{help}");
    for disclosed in [
        "`pi` imports only one `--path` session file, locally (no scan, no upload)",
        "stores its snapshot as partial and unproven",
    ] {
        let collapsed = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(collapsed.contains(disclosed), "{disclosed}: {help}");
    }
    for args in [
        &["agent", "import", "--agent", "pi", "--all", "--yes"][..],
        &[
            "agent",
            "import",
            "--agent",
            "pi",
            "--session",
            PI10_SESSION_ID,
            "--yes",
        ][..],
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success(), "{}", describe(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Pi import requires --path"),
            "{}",
            describe(&output)
        );
    }
    // A `..` step is refused with the reason, before anything is opened.
    let dotted = fixture
        .home
        .join(".pi/agent/sessions/pi10/..")
        .join(format!(
            "pi10/2026-10-31T00-00-00-000Z_{PI10_SESSION_ID}.jsonl"
        ));
    let output = pi10_import(&fixture, &dotted, &["--yes", "--json"]);
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("without `..` components"),
        "{}",
        describe(&output)
    );
    // The file name must be Pi's: `<timestamp>_<id>.jsonl`.
    let misnamed = fixture
        .home
        .join(format!("not-a-timestamp_{PI10_SESSION_ID}.jsonl"));
    std::fs::write(&misnamed, pi10_session(PI10_SESSION_ID, &fixture.repo))
        .expect("misnamed session file");
    let output = pi10_import(&fixture, &misnamed, &["--yes", "--json"]);
    assert!(!output.status.success(), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("valid provider session id"),
        "{}",
        describe(&output)
    );
    assert_eq!(pi10_session_rows(&fixture), 0);
}

/// PI-10 AC2 (stored content): each turn's checkpoint keeps only its own
/// thinking blocks, and metadata keeps only their count, so a large valid
/// thinking block stays with its turn and every checkpoint stays readable.
#[test]
fn pi_explicit_import_keeps_each_turns_thinking_with_its_checkpoint() {
    let fixture = ImportRepo::init();
    let large = "pi10-large ".repeat(200_000);
    let bytes = PiV3File::with_header(json!({
        "type": "session",
        "version": 3,
        "id": PI10_SESSION_ID,
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": fixture.repo.to_str().expect("utf-8 cwd"),
    }))
    .entry(
        "message",
        "u1",
        None,
        json!({"message": {"role": "user", "content": "pi10-first"}}),
    )
    .entry(
        "message",
        "a1",
        Some("u1"),
        json!({"message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "pi10-first-thinking"},
            {"type": "text", "text": "pi10-first-answer"},
        ]}}),
    )
    .entry(
        "message",
        "u2",
        Some("a1"),
        json!({"message": {"role": "user", "content": "pi10-second"}}),
    )
    .entry(
        "message",
        "a2",
        Some("u2"),
        json!({"message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": large},
            {"type": "text", "text": "pi10-second-answer"},
        ]}}),
    )
    .bytes();
    let path = pi10_write(&fixture, PI10_SESSION_ID, &bytes);
    let output = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(output.status.success(), "{}", describe(&output));
    assert_eq!(checkpoint_count(&fixture), 2);
    let rows = scalar_text(
        &fixture,
        "SELECT group_concat(checkpoint_id || ' ' || tree_oid, ';') FROM agent_checkpoint",
    );
    for row in rows.split(';') {
        let mut fields = row.split(' ');
        let (checkpoint_id, tree_oid) = (
            fields.next().expect("checkpoint id"),
            fields.next().expect("tree"),
        );
        let blobs: BTreeMap<String, Vec<u8>> =
            pi10_checkpoint_blobs(&fixture.repo.join(".libra"), tree_oid, checkpoint_id)
                .into_iter()
                .collect();
        let turn: Value =
            serde_json::from_slice(blobs.get("transcript/pi.jsonl").expect("the turn"))
                .expect("parse the turn");
        let key = turn["logical_turn_key"]
            .as_str()
            .expect("turn key")
            .to_string();
        let report: Value =
            serde_json::from_slice(blobs.get("redaction_report.json").expect("the report"))
                .expect("parse the report");
        let retained = report["pi_retained"].as_array().expect("retained blocks");
        assert_eq!(retained.len(), 1, "{key}: {}", retained.len());
        assert_eq!(retained[0]["turn_key"], key);
        let expected = if key == "u1" {
            json!("pi10-first-thinking")
        } else {
            json!(large)
        };
        assert!(
            retained[0]["thinking"] == expected,
            "{key} keeps exactly its own thinking, in full"
        );
        assert_eq!(report["pi_retained_blocks"], 2);
        let metadata = blobs.get("metadata.json").expect("metadata");
        assert!(
            metadata.len() < 64 * 1024,
            "{key}: metadata is {} bytes",
            metadata.len()
        );
        assert!(!String::from_utf8_lossy(metadata).contains("pi10-large"));
        let shown = fixture.run(&["agent", "checkpoint", "show", checkpoint_id, "--json"]);
        assert!(shown.status.success(), "{key}: {}", describe(&shown));
    }
}

/// PI-10 AC5: an import whose process is killed after its first durable
/// turn (it is held at its next write, then SIGKILLed, so no cleanup runs)
/// leaves the recovery state the shared coordinator owns. Once the dead
/// owner's leases have lapsed, the retry commits every turn exactly once
/// and leaves no in-flight marker; a replay commits nothing.
#[cfg(unix)]
#[tokio::test]
async fn pi_explicit_import_recovers_after_the_process_is_killed() {
    use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};

    // Enough turns that the import is still running when the lock is taken,
    // and few enough that the retry finishes inside the 120s import deadline
    // while the rest of the suite is holding the SQLite writer.
    const TURNS: usize = 40;
    let fixture = ImportRepo::init();
    let mut file = PiV3File::with_header(json!({
        "type": "session",
        "version": 3,
        "id": PI10_SESSION_ID,
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": fixture.repo.to_str().expect("utf-8 cwd"),
    }));
    let mut parent: Option<String> = None;
    for turn in 0..TURNS {
        let user = format!("u{turn}");
        let answer = format!("a{turn}");
        file = file
            .entry(
                "message",
                &user,
                parent.as_deref(),
                json!({"message": {"role": "user", "content": format!("pi10-prompt-{turn}")}}),
            )
            .entry(
                "message",
                &answer,
                Some(&user),
                json!({"message": {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": format!("pi10-thinking-{turn}")},
                    {"type": "text", "text": format!("pi10-answer-{turn}")},
                ]}}),
            );
        parent = Some(answer);
    }
    let path = pi10_write(&fixture, PI10_SESSION_ID, &file.bytes());
    let database = fixture.repo.join(".libra/libra.db");
    let mut lock = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&database)
            // The import may hold the writer lock for a while under load.
            .busy_timeout(Duration::from_secs(60)),
    )
    .await
    .expect("open the repository database");
    let path_text = path_arg(&path);
    let mut child = fixture
        .command()
        .args([
            "agent",
            "import",
            "--agent",
            "pi",
            "--path",
            path_text.as_str(),
            "--yes",
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the import");
    let started = Instant::now();
    loop {
        let committed: i64 = sea_orm::sqlx::query_scalar("SELECT COUNT(*) FROM agent_checkpoint")
            .fetch_one(&mut lock)
            .await
            .expect("count checkpoints");
        if committed >= 1 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the import committed no turn"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // Hold the writer lock so the import stops at its next write, then kill
    // it there: a crash after a durable turn, with no cleanup.
    sea_orm::sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut lock)
        .await
        .expect("take the writer lock");
    let committed: i64 = sea_orm::sqlx::query_scalar("SELECT COUNT(*) FROM agent_checkpoint")
        .fetch_one(&mut lock)
        .await
        .expect("count checkpoints");
    assert!(
        (1..TURNS as i64).contains(&committed),
        "the import must be stopped mid-way, after {committed} of {TURNS} turns"
    );
    child.kill().expect("kill the import");
    let status = child.wait().expect("reap the import");
    assert!(!status.success());
    // Stands in for the time the dead owner's leases and in-flight marker
    // take to lapse.
    sea_orm::sqlx::query("UPDATE agent_import_identity SET lease_expires_at = 0")
        .execute(&mut lock)
        .await
        .expect("lapse the identity lease");
    sea_orm::sqlx::query(
        "UPDATE metadata_kv SET value = json_set(value, '$.lease_expires_at', 0)
         WHERE scope = 'agent_import_index_repair'",
    )
    .execute(&mut lock)
    .await
    .expect("lapse the index barrier lease");
    sea_orm::sqlx::query(
        "UPDATE metadata_kv SET value = json_set(value, '$.started_at_ms', 0)
         WHERE scope = 'agent_traces_inflight'",
    )
    .execute(&mut lock)
    .await
    .expect("lapse the in-flight marker");
    sea_orm::sqlx::query("COMMIT")
        .execute(&mut lock)
        .await
        .expect("release the writer lock");
    lock.close().await.expect("close the repository database");

    let retried = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(retried.status.success(), "{}", describe(&retried));
    assert_eq!(
        checkpoint_count(&fixture),
        TURNS as i64,
        "a turn was committed twice or lost"
    );
    assert_eq!(identity_state(&fixture), "committed");
    assert_eq!(
        scalar(
            &fixture,
            "SELECT COUNT(*) FROM metadata_kv WHERE scope = 'agent_traces_inflight'"
        ),
        0,
        "the dead owner's in-flight marker is retired"
    );
    let replayed = pi10_import(&fixture, &path, &["--yes", "--json"]);
    assert!(replayed.status.success(), "{}", describe(&replayed));
    assert_eq!(checkpoint_count(&fixture), TURNS as i64);
}
