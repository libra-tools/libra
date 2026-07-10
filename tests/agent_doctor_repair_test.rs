//! AG-20 `libra agent doctor [--repair]` three-class detection/repair
//! contract (plan.md Task A5, `agent.md` doctor repair matrix).
//!
//! Drives the built `libra` binary end-to-end: `libra init` in a tempdir,
//! real checkpoint ingestion through `libra agent hooks claude-code …`,
//! then direct DB/object-store manipulation to fabricate each inconsistency
//! class, and finally `libra agent doctor [--json] [--repair]` assertions:
//!
//! - **class 2** (`missing_catalog_row`): DELETE the `agent_checkpoint` row
//!   of a ref-reachable checkpoint → detected; `--repair` re-INSERTs it with
//!   the original key fields (reconstructed from the commit's metadata.json
//!   + `Libra-*` trailers); a second run is clean (idempotent).
//! - **class 1** (`stale_catalog_row` / `missing_objects`): corrupting the
//!   row's OID columns → repaired back from `refs/libra/traces`; deleting
//!   the objects themselves (ref gone) → `missing_objects`, manual only,
//!   row untouched by `--repair`. E4 sidecar coverage: deleting a single
//!   sidecar blob (`redaction_report.json`) or the `manifest.json` blob
//!   itself → `missing_objects` naming the sidecar (never `legacy-v1`),
//!   with the remaining sidecars still checked, and a healthy report only
//!   after the object returns.
//! - **class 3** (`missing_object_index`): DELETE the `object_index` rows
//!   of checkpoint objects → `--repair` re-inserts them idempotently with
//!   the writer's row semantics (payload size — manifest-declared
//!   `byte_len` for declared roles, so transcript payloads are never read;
//!   o_type commit/tree/blob and `agent_transcript` for the transcript
//!   blob), compared against the writer-enqueued baseline rows — for the
//!   row-column OIDs and for sidecar blobs alike. A checkpoint with BOTH a
//!   stale row and missing index rows is fully fixed by one `--repair`
//!   run (auto-repairable class-1 findings do not suppress class 3).
//! - **legacy-v1**: the committed fixture
//!   (`tests/fixtures/agent_checkpoints/v1_claude_code`) seeded as a real
//!   traces commit is classified `legacy_v1_checkpoints`, never enters the
//!   three classes, and is byte-identical after `--repair`.
//! - **orphan rule fidelity**: session-without-checkpoint is legal and
//!   never flagged.
//! - **gemini**: leftover hook config yields the uninstall-channel hint;
//!   captured gemini rows are read-only data and never flagged.
//! - **span coverage**: `--repair` emits `agent.doctor.repair` with the §6
//!   required fields (asserted via `LIBRA_LOG_FILE`); detection-only runs
//!   emit none, and no transcript content ever reaches the sink.

#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One isolated libra repository plus a fake `$HOME` for the Claude Code
/// transcript root (`~/.claude`). Mirrors `tests/agent_lifecycle_event_test.rs`.
struct DoctorRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl DoctorRepo {
    fn init() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let this = Self {
            _tempdir: tempdir,
            repo,
            home,
        };
        let out = this.run(&["init"], None);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    /// Run the built `libra` binary inside the repo with a clean
    /// environment (plus optional extra env vars, e.g. LIBRA_LOG*).
    fn run_env(&self, args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_COMMITTER_NAME", "Doctor Test")
            .env("LIBRA_COMMITTER_EMAIL", "doctor@test.libra")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn libra binary");
        if let Some(payload) = stdin {
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write hook envelope to stdin");
        }
        child.wait_with_output().expect("wait for libra binary")
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_env(args, stdin, &[])
    }

    /// `libra agent hooks <agent> <verb>` with `envelope` piped via stdin.
    fn hook(&self, agent: &str, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope))
    }

    fn envelope(&self, hook_event_name: &str, session_id: &str, transcript: &Path) -> String {
        json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
            "transcript_path": transcript.to_string_lossy(),
        })
        .to_string()
    }

    /// Write a Claude Code transcript under the fake home so the writer's
    /// provider-root trust gate accepts it.
    fn write_claude_transcript(&self, content: &str) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("x");
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, content).expect("write transcript fixture");
        path
    }

    /// SessionStart + Stop for one provider session id — ingests exactly
    /// one committed checkpoint through the real writer.
    fn ingest_checkpoint(&self, session: &str, transcript_content: &str) {
        let transcript = self.write_claude_transcript(transcript_content);
        let out = self.hook(
            "claude-code",
            "session-start",
            &self.envelope("SessionStart", session, &transcript),
        );
        assert!(out.status.success(), "session-start: {}", describe(&out));
        let out = self.hook(
            "claude-code",
            "stop",
            &self.envelope("Stop", session, &transcript),
        );
        assert!(out.status.success(), "stop: {}", describe(&out));
    }

    /// Fresh sea-orm connection to the repo database. Callers drop it
    /// before the next CLI invocation (sequential access only).
    async fn db(&self) -> DatabaseConnection {
        let db_url = format!(
            "sqlite://{}",
            self.repo.join(".libra").join("libra.db").display()
        );
        let mut opts = sea_orm::ConnectOptions::new(db_url);
        opts.sqlx_logging(false);
        sea_orm::Database::connect(opts)
            .await
            .expect("open repo db")
    }

    async fn exec_sql(&self, sql: &str, values: Vec<sea_orm::Value>) {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(backend, sql, values))
            .await
            .expect("execute test SQL");
    }

    /// All `agent_checkpoint` rows, oldest first.
    async fn checkpoint_rows(&self) -> Vec<RowSnapshot> {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        let rows = conn
            .query_all(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                        metadata_blob_oid, traces_commit, created_at \
                 FROM agent_checkpoint ORDER BY created_at ASC, checkpoint_id ASC",
                [],
            ))
            .await
            .expect("query agent_checkpoint");
        rows.into_iter()
            .map(|row| RowSnapshot {
                checkpoint_id: row.try_get_by("checkpoint_id").unwrap(),
                session_id: row.try_get_by("session_id").unwrap(),
                scope: row.try_get_by("scope").unwrap(),
                parent_commit: row.try_get_by("parent_commit").ok().flatten(),
                tree_oid: row.try_get_by("tree_oid").unwrap(),
                metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap(),
                traces_commit: row.try_get_by("traces_commit").unwrap(),
                created_at: row.try_get_by("created_at").unwrap(),
            })
            .collect()
    }

    /// `object_index` rows for the given OIDs as `(o_id, o_type, o_size,
    /// repo_id)`, ordered by o_id.
    async fn object_index_rows(&self, oids: &[&str]) -> Vec<(String, String, i64, String)> {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        let placeholders = vec!["?"; oids.len()].join(", ");
        let sql = format!(
            "SELECT o_id, o_type, o_size, repo_id FROM object_index \
             WHERE o_id IN ({placeholders}) ORDER BY o_id ASC"
        );
        let values: Vec<sea_orm::Value> = oids.iter().map(|oid| (*oid).into()).collect();
        let rows = conn
            .query_all(Statement::from_sql_and_values(backend, sql, values))
            .await
            .expect("query object_index");
        rows.into_iter()
            .map(|row| {
                (
                    row.try_get_by("o_id").unwrap(),
                    row.try_get_by("o_type").unwrap(),
                    row.try_get_by("o_size").unwrap(),
                    row.try_get_by("repo_id").unwrap(),
                )
            })
            .collect()
    }

    /// Run `libra agent doctor [--repair] --json` and return the `data`
    /// object of the CLI envelope.
    fn doctor_json(&self, repair: bool) -> Value {
        let mut args = vec!["agent", "doctor"];
        if repair {
            args.push("--repair");
        }
        args.push("--json");
        let out = self.run(&args, None);
        assert!(out.status.success(), "doctor failed: {}", describe(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("doctor stdout is not JSON ({err}): {stdout}"));
        assert_eq!(parsed["ok"], json!(true), "envelope not ok: {parsed}");
        parsed["data"].clone()
    }

    /// Bytes of one loose object file in the repo store.
    fn loose_object_bytes(&self, oid: &str) -> Vec<u8> {
        let path = self
            .repo
            .join(".libra")
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read loose object {oid}: {e}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RowSnapshot {
    checkpoint_id: String,
    session_id: String,
    scope: String,
    parent_commit: Option<String>,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
    created_at: i64,
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn findings(report: &Value) -> Vec<Value> {
    report["checkpoint_store"]["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("checkpoint_store.findings missing: {report}"))
        .clone()
}

fn assert_store_clean(report: &Value) {
    assert_eq!(
        findings(report).len(),
        0,
        "expected a clean checkpoint store: {report}"
    );
    assert_eq!(report["checkpoint_store"]["repaired"], json!(0));
    assert_eq!(report["checkpoint_store"]["manual_required"], json!(0));
}

const TRANSCRIPT: &str =
    "{\"type\":\"user\",\"text\":\"hello doctor\"}\n{\"type\":\"assistant\",\"text\":\"done\"}\n";

// ---------------------------------------------------------------------------
// Loose-object reading helpers (E4 tree navigation for sidecar tests)
// ---------------------------------------------------------------------------

/// Read + zlib-decode one loose object, returning `(type, body)`.
fn read_loose(repo: &DoctorRepo, oid: &str) -> (String, Vec<u8>) {
    let raw = repo.loose_object_bytes(oid);
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).expect("zlib decode");
    let header_end = decoded
        .iter()
        .position(|&b| b == 0)
        .expect("object header terminator");
    let header = std::str::from_utf8(&decoded[..header_end]).expect("utf8 header");
    let object_type = header
        .split(' ')
        .next()
        .expect("object type in header")
        .to_string();
    (object_type, decoded[header_end + 1..].to_vec())
}

/// Parse a (SHA-1) tree object's entries as `(mode, name, oid)`.
fn tree_entries(repo: &DoctorRepo, oid: &str) -> Vec<(String, String, String)> {
    let (object_type, body) = read_loose(repo, oid);
    assert_eq!(object_type, "tree", "object {oid} must be a tree");
    let mut entries = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let space = cursor
            + body[cursor..]
                .iter()
                .position(|&b| b == b' ')
                .expect("mode terminator");
        let mode = std::str::from_utf8(&body[cursor..space])
            .expect("utf8 mode")
            .to_string();
        let name_start = space + 1;
        let null = name_start
            + body[name_start..]
                .iter()
                .position(|&b| b == 0)
                .expect("name terminator");
        let name = std::str::from_utf8(&body[name_start..null])
            .expect("utf8 name")
            .to_string();
        let hash_start = null + 1;
        let entry_oid = hex::encode(&body[hash_start..hash_start + 20]);
        entries.push((mode, name, entry_oid));
        cursor = hash_start + 20;
    }
    entries
}

fn subtree_oid(repo: &DoctorRepo, tree: &str, name: &str) -> String {
    tree_entries(repo, tree)
        .into_iter()
        .find(|(mode, entry_name, _)| entry_name == name && mode == "40000")
        .unwrap_or_else(|| panic!("tree entry '{name}' missing from tree {tree}"))
        .2
}

/// All blob entries of the checkpoint's inner E4 tree as
/// `path → oid` pairs (top-level sidecars plus `events/…` and
/// `transcript/…` leaves).
fn checkpoint_sidecars(repo: &DoctorRepo, row: &RowSnapshot) -> Vec<(String, String)> {
    let checkpoint = subtree_oid(repo, &row.tree_oid, "checkpoint");
    let prefix = subtree_oid(repo, &checkpoint, &row.checkpoint_id[..2]);
    let inner = subtree_oid(repo, &prefix, &row.checkpoint_id[2..]);
    let mut out = Vec::new();
    for (mode, name, oid) in tree_entries(repo, &inner) {
        if mode == "40000" {
            for (_, leaf_name, leaf_oid) in tree_entries(repo, &oid) {
                out.push((format!("{name}/{leaf_name}"), leaf_oid));
            }
        } else {
            out.push((name, oid));
        }
    }
    out
}

fn sidecar_oid(repo: &DoctorRepo, row: &RowSnapshot, path: &str) -> String {
    checkpoint_sidecars(repo, row)
        .into_iter()
        .find(|(entry_path, _)| entry_path == path)
        .unwrap_or_else(|| panic!("sidecar '{path}' missing from checkpoint tree"))
        .1
}

fn delete_loose_object(repo: &DoctorRepo, oid: &str) -> Vec<u8> {
    let path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read loose object {oid}: {e}"));
    std::fs::remove_file(&path).unwrap_or_else(|e| panic!("delete loose object {oid}: {e}"));
    bytes
}

fn restore_loose_object(repo: &DoctorRepo, oid: &str, bytes: &[u8]) {
    let path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("restore loose object {oid}: {e}"));
}

// ---------------------------------------------------------------------------
// Class 2 — ref-reachable commit without catalog row (crash window B)
// ---------------------------------------------------------------------------

/// Ingest a checkpoint, DELETE its `agent_checkpoint` row: doctor detects
/// `missing_catalog_row`; `--repair` re-INSERTs the row with the original
/// key fields (including `parent_commit`, reconstructed from the
/// `Libra-Parent-Commit` trailer); a second run of either mode is clean.
#[tokio::test]
async fn class2_missing_catalog_row_detected_and_repaired() {
    let repo = DoctorRepo::init();

    // A user commit first, so the checkpoint's parent_commit is Some(head)
    // and the class-2 trailer reconstruction path is exercised end-to-end.
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    let out = repo.run(&["add", "seed.txt"], None);
    assert!(out.status.success(), "libra add: {}", describe(&out));
    let out = repo.run(&["commit", "-m", "seed"], None);
    assert!(out.status.success(), "libra commit: {}", describe(&out));

    repo.ingest_checkpoint("sess-class2", TRANSCRIPT);
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 1, "expected exactly one checkpoint: {rows:?}");
    let original = rows[0].clone();
    assert!(
        original.parent_commit.is_some(),
        "with a user commit the checkpoint must record a parent_commit"
    );

    // Baseline: healthy store.
    assert_store_clean(&repo.doctor_json(false));

    // Fabricate window B: ref advanced, catalog row missing.
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![original.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: reported, not repaired, nothing written back.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["inconsistency_type"], json!("missing_catalog_row"));
    assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(found[0]["repaired"], json!(false));
    assert_eq!(found[0]["manual_required"], json!(false));
    assert_eq!(report["checkpoint_store"]["repair_applied"], json!(false));
    assert!(
        repo.checkpoint_rows().await.is_empty(),
        "detection-only must not write the row back"
    );

    // Repair: the row comes back, equal on every key field.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["repaired"], json!(true));
    assert_eq!(report["checkpoint_store"]["repaired"], json!(1));
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 1, "repair must reinsert exactly one row");
    assert_eq!(
        rows[0], original,
        "repaired row must match the original on all key fields"
    );

    // Idempotency: both modes are now clean no-ops.
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(repo.checkpoint_rows().await, vec![original]);
}

// ---------------------------------------------------------------------------
// Class 1 — DB row vs object store / ref truth
// ---------------------------------------------------------------------------

/// Corrupt all three OID columns of a ref-reachable row: doctor reports
/// `stale_catalog_row`; `--repair` rebuilds the columns from
/// `refs/libra/traces`; second run clean.
#[tokio::test]
async fn class1_stale_row_repaired_from_ref() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class1", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);

    repo.exec_sql(
        "UPDATE agent_checkpoint SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? \
         WHERE checkpoint_id = ?",
        vec![
            "a".repeat(40).into(),
            "b".repeat(40).into(),
            "c".repeat(40).into(),
            original.checkpoint_id.clone().into(),
        ],
    )
    .await;

    // Detection-only: reported as auto-repairable, row left corrupt.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["inconsistency_type"], json!("stale_catalog_row"));
    assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(found[0]["manual_required"], json!(false));
    assert_eq!(found[0]["repaired"], json!(false));
    let still_corrupt = repo.checkpoint_rows().await.remove(0);
    assert_eq!(still_corrupt.tree_oid, "a".repeat(40));

    // Repair restores every OID column from the ref-reachable commit.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["repaired"], json!(true));
    let repaired = repo.checkpoint_rows().await.remove(0);
    assert_eq!(
        repaired, original,
        "repair must restore tree_oid/metadata_blob_oid/traces_commit from the ref"
    );

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// A checkpoint suffering BOTH a stale catalog row (class 1,
/// auto-repairable) and missing `object_index` rows (class 3) must be
/// fully fixed by a SINGLE `doctor --repair` run — an auto-repairable
/// stale finding must not suppress the ref-side class-3 check, or
/// cloud-sync visibility would stay broken until a second invocation.
#[tokio::test]
async fn stale_row_and_missing_object_index_fixed_in_single_repair() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-stale-and-index", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let oids = [
        original.traces_commit.as_str(),
        original.tree_oid.as_str(),
        original.metadata_blob_oid.as_str(),
    ];
    let index_before = repo.object_index_rows(&oids).await;
    assert_eq!(index_before.len(), 3, "writer baseline: {index_before:?}");

    // Fabricate both inconsistencies on the same checkpoint: corrupt the
    // row's OID columns AND drop the object_index rows of the real
    // (ref-side) objects.
    repo.exec_sql(
        "UPDATE agent_checkpoint SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? \
         WHERE checkpoint_id = ?",
        vec![
            "a".repeat(40).into(),
            "b".repeat(40).into(),
            "c".repeat(40).into(),
            original.checkpoint_id.clone().into(),
        ],
    )
    .await;
    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection sees both findings, both auto-repairable, and the class-3
    // finding names the REF-side (real) OIDs, not the corrupt columns.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 2, "both findings expected: {report}");
    let stale = found
        .iter()
        .find(|f| f["inconsistency_type"] == json!("stale_catalog_row"))
        .unwrap_or_else(|| panic!("stale finding missing: {report}"));
    let index = found
        .iter()
        .find(|f| f["inconsistency_type"] == json!("missing_object_index"))
        .unwrap_or_else(|| panic!("class-3 finding missing: {report}"));
    assert_eq!(stale["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(index["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(report["checkpoint_store"]["manual_required"], json!(0));
    let detail = index["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(
            detail.contains(*oid),
            "class-3 detail must name the ref-side OID {oid}: {detail}"
        );
    }
    assert!(
        !detail.contains(&"a".repeat(40)),
        "class-3 must not target the corrupt column values: {detail}"
    );

    // ONE --repair run fixes both.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 2, "both findings expected: {report}");
    assert!(
        found.iter().all(|f| f["repaired"] == json!(true)),
        "a single --repair must fix the stale row AND the index rows: {report}"
    );
    assert_eq!(report["checkpoint_store"]["repaired"], json!(2));
    assert_eq!(
        repo.checkpoint_rows().await,
        vec![original.clone()],
        "row restored from the ref"
    );
    let mut index_after = repo.object_index_rows(&oids).await;
    index_after.sort();
    let mut expected = index_before.clone();
    expected.sort();
    assert_eq!(
        index_after, expected,
        "object_index rows restored to the writer baseline in the same run"
    );

    // Second runs are clean no-ops.
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// Genuinely missing objects (and no ref to rebuild from) are reported as
/// `missing_objects`, require manual action, and are never "repaired" by
/// destructive means — the row survives `--repair` untouched.
#[tokio::test]
async fn class1_missing_objects_reported_manual_only() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-missing", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);

    // Drop the traces ref (nothing ref-reachable any more) and delete the
    // metadata blob object from the store.
    repo.exec_sql(
        "DELETE FROM reference WHERE name = ? AND kind = 'Branch'",
        vec!["traces".into()],
    )
    .await;
    let blob_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&original.metadata_blob_oid[..2])
        .join(&original.metadata_blob_oid[2..]);
    std::fs::remove_file(&blob_path).expect("delete metadata blob object");

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        assert!(
            found[0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains(&original.metadata_blob_oid),
            "detail must name the missing object: {report}"
        );
        assert_eq!(report["checkpoint_store"]["manual_required"], json!(1));
        // No destructive action: the row is still exactly as written.
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }
}

// ---------------------------------------------------------------------------
// Class 3 — object_index rows missing for catalog-known OIDs
// ---------------------------------------------------------------------------

/// DELETE the `object_index` rows of a checkpoint's three OIDs: doctor
/// detects `missing_object_index`; `--repair` re-inserts rows equivalent
/// to the writer's enqueue (same o_type/o_size/repo_id); second run clean.
#[tokio::test]
async fn class3_missing_object_index_reinserted() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class3", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let oids = [
        row.traces_commit.as_str(),
        row.tree_oid.as_str(),
        row.metadata_blob_oid.as_str(),
    ];

    // Baseline: the writer's background indexer catalogued all three
    // (the CLI drains its index queue before exiting).
    let before = repo.object_index_rows(&oids).await;
    assert_eq!(
        before.len(),
        3,
        "expected object_index rows for commit/tree/metadata: {before:?}"
    );

    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection-only.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    assert_eq!(found[0]["checkpoint_id"], json!(row.checkpoint_id));
    assert_eq!(found[0]["manual_required"], json!(false));
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(detail.contains(*oid), "detail must list {oid}: {detail}");
    }
    assert!(
        repo.object_index_rows(&oids).await.is_empty(),
        "detection-only must not reinsert object_index rows"
    );

    // Repair: rows come back with the writer's semantics.
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    let mut after = repo.object_index_rows(&oids).await;
    after.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(
        after, expected,
        "repaired rows must match the writer-enqueued rows on o_id/o_type/o_size/repo_id"
    );

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(
        repo.object_index_rows(&oids).await.len(),
        3,
        "second repair run must not duplicate rows"
    );
}

/// A single missing E4 sidecar blob (`redaction_report.json`) — the row
/// columns are all intact — is a class-1 `missing_objects` finding naming
/// the sidecar; `--repair` cannot resurrect a lost blob (manual only, no
/// destructive action), and the store reports healthy again only once the
/// object returns.
#[tokio::test]
async fn class1_missing_sidecar_blob_detected_manual_only() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-sidecar", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let report_oid = sidecar_oid(&repo, &original, "redaction_report.json");
    let saved = delete_loose_object(&repo, &report_oid);

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        let detail = found[0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("redaction_report.json") && detail.contains(&report_oid),
            "detail must name the missing sidecar and its OID: {detail}"
        );
        // Not a legacy misclassification, and no destructive action.
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(0)
        );
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }

    // Healthy again only after the object is restored.
    restore_loose_object(&repo, &report_oid, &saved);
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// A missing `manifest.json` blob is class-1 `missing_objects` — NOT
/// legacy-v1 (the tree entry still exists; legacy means the entry is
/// absent) — and one missing manifest must not hide other missing
/// sidecars: a simultaneously deleted `content_hash.txt` is named too.
#[tokio::test]
async fn class1_missing_manifest_is_not_legacy_and_other_sidecars_still_checked() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-manifest", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let manifest_oid = sidecar_oid(&repo, &original, "manifest.json");
    let hash_oid = sidecar_oid(&repo, &original, "content_hash.txt");
    delete_loose_object(&repo, &manifest_oid);
    delete_loose_object(&repo, &hash_oid);

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(0),
            "a missing manifest blob must never classify as legacy-v1: {report}"
        );
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        let detail = found[0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("manifest.json") && detail.contains(&manifest_oid),
            "detail must name the missing manifest: {detail}"
        );
        assert!(
            detail.contains("content_hash.txt") && detail.contains(&hash_oid),
            "a missing manifest must not hide other missing sidecars: {detail}"
        );
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }
}

/// DELETE the `object_index` rows of E4 sidecar objects (manifest,
/// lifecycle events, transcript): doctor detects `missing_object_index`
/// and `--repair` re-inserts rows equal to the writer-enqueued baseline —
/// in particular the transcript blob keeps the writer's distinguished
/// `agent_transcript` o_type.
#[tokio::test]
async fn class3_missing_sidecar_object_index_rows_reinserted() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-sidecar-index", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let manifest_oid = sidecar_oid(&repo, &row, "manifest.json");
    let events_oid = sidecar_oid(&repo, &row, "events/lifecycle.jsonl");
    let transcript_oid = sidecar_oid(&repo, &row, "transcript/claude_code.jsonl");
    let oids = [
        manifest_oid.as_str(),
        events_oid.as_str(),
        transcript_oid.as_str(),
    ];

    // Writer-enqueued baseline (the CLI drains its index queue on exit).
    let before = repo.object_index_rows(&oids).await;
    assert_eq!(
        before.len(),
        3,
        "expected object_index rows for the sidecar blobs: {before:?}"
    );
    let transcript_row = before
        .iter()
        .find(|(o_id, ..)| *o_id == transcript_oid)
        .expect("transcript object_index row");
    assert_eq!(
        transcript_row.1, "agent_transcript",
        "writer tags the transcript blob as agent_transcript: {before:?}"
    );

    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection-only names every missing sidecar OID.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    assert_eq!(found[0]["checkpoint_id"], json!(row.checkpoint_id));
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(detail.contains(*oid), "detail must list {oid}: {detail}");
    }

    // Repair restores the exact writer baseline (o_id/o_type/o_size/repo_id).
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    let mut after = repo.object_index_rows(&oids).await;
    after.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(
        after, expected,
        "repaired sidecar rows must match the writer-enqueued baseline"
    );

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(
        repo.object_index_rows(&oids).await.len(),
        3,
        "second repair run must not duplicate rows"
    );
}

// ---------------------------------------------------------------------------
// Legacy-v1 — exempt from all classes, byte-identical across --repair
// ---------------------------------------------------------------------------

/// Everything seeded for the v1 fixture checkpoint: object OIDs (for the
/// byte-identity check) plus its catalog row.
struct V1Seed {
    checkpoint_id: String,
    object_oids: Vec<String>,
}

/// Seed the committed v1-layout fixture
/// (`tests/fixtures/agent_checkpoints/v1_claude_code`) into `repo` as a
/// real root commit on `refs/libra/traces`: byte-identical blobs (OIDs
/// re-verified against the fixture README pins), reconstructed v1 trees
/// (`metadata.json` + `transcript/claude_code`, NO manifest.json), a
/// traces commit with `Libra-*` trailers, the `reference` row, and the
/// `agent_session` / `agent_checkpoint` rows the v1 writer would have
/// written. Deliberately does NOT seed `object_index` rows — legacy-v1
/// checkpoints are exempt from class 3 too.
fn seed_v1_fixture(repo: &DoctorRepo) -> V1Seed {
    let libra_dir = repo.repo.join(".libra");
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/agent_checkpoints/v1_claude_code/85/ae75d2-4c53-465a-b890-a9f861a50cc7",
    );
    let checkpoint_id = "85ae75d2-4c53-465a-b890-a9f861a50cc7".to_string();
    let session_id = "claude__fixture-v1-claude";

    let metadata_bytes =
        std::fs::read(fixture_root.join("metadata.json")).expect("fixture metadata");
    let transcript_bytes =
        std::fs::read(fixture_root.join("transcript/claude_code")).expect("fixture transcript");

    let write_blob = |bytes: &[u8]| -> ObjectHash {
        libra::utils::object::write_git_object(&libra_dir, "blob", bytes)
            .expect("write fixture blob")
    };
    let metadata_oid = write_blob(&metadata_bytes);
    assert_eq!(
        metadata_oid.to_string(),
        "b0265e8c5249c53dc588913554cdebdb82b984ec",
        "fixture metadata.json must rehash to the README-pinned OID"
    );
    let transcript_oid = write_blob(&transcript_bytes);
    assert_eq!(
        transcript_oid.to_string(),
        "2c43a69258d78142464f074e4c050bd9c7f0325f",
        "fixture transcript must rehash to the README-pinned OID"
    );

    let write_tree = |items: Vec<TreeItem>| -> ObjectHash {
        let tree = Tree::from_tree_items(items).expect("build tree");
        let data = tree.to_data().expect("serialize tree");
        libra::utils::object::write_git_object(&libra_dir, "tree", &data)
            .expect("write fixture tree")
    };
    // v1 inner layout: metadata.json + transcript/<provider> (no
    // extension), and crucially NO manifest.json.
    let transcript_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Blob,
        transcript_oid,
        "claude_code".to_string(),
    )]);
    let inner_tree = write_tree(vec![
        TreeItem::new(
            TreeItemMode::Blob,
            metadata_oid,
            "metadata.json".to_string(),
        ),
        TreeItem::new(
            TreeItemMode::Tree,
            transcript_tree,
            "transcript".to_string(),
        ),
    ]);
    let prefix_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        inner_tree,
        checkpoint_id[2..].to_string(),
    )]);
    let checkpoint_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        prefix_tree,
        checkpoint_id[..2].to_string(),
    )]);
    let root_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        checkpoint_tree,
        "checkpoint".to_string(),
    )]);

    let message = format!(
        "traces: committed checkpoint {checkpoint_id}\n\n\
         Libra-Session: {session_id}\n\
         Libra-Agent: claude_code\n\
         Libra-Checkpoint-ID: {checkpoint_id}\n\
         Libra-Scope: committed\n"
    );
    let author = Signature::new(
        SignatureType::Author,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let committer = Signature::new(
        SignatureType::Committer,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let commit = Commit::new(author, committer, root_tree, vec![], &message);
    let commit_data = commit.to_data().expect("serialize fixture commit");
    let commit_oid = libra::utils::object::write_git_object(&libra_dir, "commit", &commit_data)
        .expect("write fixture commit");

    V1Seed {
        checkpoint_id,
        object_oids: vec![
            metadata_oid.to_string(),
            transcript_oid.to_string(),
            transcript_tree.to_string(),
            inner_tree.to_string(),
            prefix_tree.to_string(),
            checkpoint_tree.to_string(),
            root_tree.to_string(),
            commit_oid.to_string(),
        ],
    }
}

async fn seed_v1_rows(repo: &DoctorRepo, seed: &V1Seed) {
    let session_id = "claude__fixture-v1-claude";
    let root_tree = &seed.object_oids[6];
    let commit_oid = &seed.object_oids[7];
    // `libra init` pre-seeds the traces branch row (commit NULL); the
    // unique index on (name, kind) is partial (WHERE remote IS NULL), so
    // update-then-insert instead of ON CONFLICT.
    repo.exec_sql(
        "UPDATE reference SET \"commit\" = ? \
         WHERE name = 'traces' AND kind = 'Branch' AND remote IS NULL",
        vec![commit_oid.clone().into()],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO reference (name, kind, \"commit\") \
         SELECT 'traces', 'Branch', ? \
         WHERE NOT EXISTS (SELECT 1 FROM reference \
                           WHERE name = 'traces' AND kind = 'Branch' AND remote IS NULL)",
        vec![commit_oid.clone().into()],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) VALUES (?, 'claude_code', ?, 'stopped', ?, 1, 1)",
        vec![
            session_id.into(),
            "fixture-v1-claude".into(),
            repo.repo.display().to_string().into(),
        ],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, ?, 'committed', NULL, ?, ?, ?, 1783206712)",
        vec![
            seed.checkpoint_id.clone().into(),
            session_id.into(),
            root_tree.clone().into(),
            seed.object_oids[0].clone().into(),
            commit_oid.clone().into(),
        ],
    )
    .await;
}

/// The v1 fixture (seeded as a real traces root commit, with a v2
/// checkpoint ingested on top of it) is classified `legacy_v1_checkpoints`,
/// never enters the three classes even though its `object_index` rows are
/// absent, and survives `--repair` byte-identical (objects, catalog row,
/// and ref all unchanged).
#[tokio::test]
async fn legacy_v1_fixture_classified_and_never_repaired() {
    let repo = DoctorRepo::init();
    let seed = seed_v1_fixture(&repo);
    seed_v1_rows(&repo, &seed).await;

    // A real v2 checkpoint on top — the writer splices onto the v1 parent,
    // proving mixed v1/v2 chains classify correctly.
    repo.ingest_checkpoint("sess-v2-on-legacy", TRANSCRIPT);
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 2, "v1 fixture + v2 ingest expected: {rows:?}");
    let v1_row = rows
        .iter()
        .find(|r| r.checkpoint_id == seed.checkpoint_id)
        .expect("v1 row present")
        .clone();

    let object_bytes_before: Vec<Vec<u8>> = seed
        .object_oids
        .iter()
        .map(|oid| repo.loose_object_bytes(oid))
        .collect();

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(1),
            "v1 fixture must be classified legacy-v1: {report}"
        );
        assert_eq!(
            report["checkpoint_store"]["ref_reachable_checkpoints"],
            json!(2),
            "both checkpoints are ref-reachable: {report}"
        );
        let found = findings(&report);
        assert!(
            found.is_empty(),
            "legacy-v1 must not enter the three classes (and the v2 \
             checkpoint is healthy): {report}"
        );
    }

    // Byte-identity: --repair must not have rewritten anything of the v1
    // checkpoint — objects, catalog row, and its object_index absence.
    for (oid, before) in seed.object_oids.iter().zip(&object_bytes_before) {
        assert_eq!(
            &repo.loose_object_bytes(oid),
            before,
            "fixture object {oid} must be byte-identical after --repair"
        );
    }
    let rows = repo.checkpoint_rows().await;
    let v1_after = rows
        .iter()
        .find(|r| r.checkpoint_id == seed.checkpoint_id)
        .expect("v1 row still present");
    assert_eq!(
        *v1_after, v1_row,
        "the legacy-v1 catalog row must be untouched by --repair"
    );
    let v1_oids: Vec<&str> = seed.object_oids.iter().map(String::as_str).collect();
    assert!(
        repo.object_index_rows(&v1_oids).await.is_empty(),
        "legacy-v1 objects are exempt from class-3 re-enqueue"
    );
}

// ---------------------------------------------------------------------------
// Orphan rule fidelity + gemini hint
// ---------------------------------------------------------------------------

/// A session without any checkpoint is a LEGAL intermediate state (active
/// session before its first Stop/TurnEnd): doctor must not flag it in any
/// category.
#[tokio::test]
async fn session_without_checkpoint_is_never_flagged() {
    let repo = DoctorRepo::init();
    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", "sess-legal-orphan", &transcript),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    let report = repo.doctor_json(false);
    assert_eq!(report["active_sessions"], json!(1));
    assert_eq!(
        report["orphan_checkpoints"],
        json!(0),
        "session-without-checkpoint must not count as orphan: {report}"
    );
    assert_store_clean(&report);
    assert_eq!(report["gemini_hooks_remnant"], json!(false));
}

/// Replicate the exact settings shape `libra agent enable gemini` used to
/// write (`hooksConfig.enabled` + the seven Libra-managed hook entries
/// pointing at the current binary), so `hooks_are_installed()` reports
/// remnants.
fn write_gemini_remnant_settings(repo: &DoctorRepo) {
    let binary =
        std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).expect("canonicalize libra binary path");
    let binary = binary.display();
    let entry = |matcher: Option<&str>, name: &str, subcommand: &str| -> Value {
        let mut obj = json!({
            "hooks": [{
                "name": name,
                "type": "command",
                "command": format!("{binary} hooks gemini {subcommand}"),
            }],
        });
        if let Some(matcher) = matcher {
            obj["matcher"] = json!(matcher);
        }
        json!([obj])
    };
    let settings = json!({
        "hooksConfig": { "enabled": true },
        "hooks": {
            "SessionStart": entry(None, "libra-session-start", "session-start"),
            "BeforeAgent": entry(None, "libra-before-agent", "prompt"),
            "AfterTool": entry(Some("*"), "libra-after-tool", "tool-use"),
            "AfterAgent": entry(None, "libra-after-agent", "stop"),
            "SessionEnd": entry(None, "libra-session-end", "session-end"),
            "BeforeModel": entry(None, "libra-before-model", "model-update"),
            "PreCompress": entry(None, "libra-pre-compress", "compaction"),
        },
    });
    let gemini_dir = repo.repo.join(".gemini");
    std::fs::create_dir_all(&gemini_dir).expect("create .gemini dir");
    std::fs::write(
        gemini_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).expect("serialize gemini settings"),
    )
    .expect("write gemini settings remnant");
}

/// Leftover gemini hook configuration triggers the uninstall-channel hint
/// (`libra agent remove gemini`); existing gemini `agent_session` rows are
/// legal read-only data and produce no findings.
#[tokio::test]
async fn gemini_remnant_hint_and_readonly_rows() {
    let repo = DoctorRepo::init();
    write_gemini_remnant_settings(&repo);
    // Legal read-only capture data from the gemini era.
    repo.exec_sql(
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) VALUES (?, 'gemini', ?, 'stopped', ?, 1, 1)",
        vec![
            "gemini__legacy-1".into(),
            "legacy-1".into(),
            repo.repo.display().to_string().into(),
        ],
    )
    .await;

    let report = repo.doctor_json(false);
    assert_eq!(
        report["gemini_hooks_remnant"],
        json!(true),
        "remnant gemini hooks must be surfaced: {report}"
    );
    let gemini_hook = report["provider_hooks"]
        .as_array()
        .expect("provider_hooks array")
        .iter()
        .find(|ph| ph["name"] == json!("gemini"))
        .expect("gemini provider row")
        .clone();
    assert_eq!(gemini_hook["installed"], json!(true));
    assert_store_clean(&report);
    assert_eq!(
        report["orphan_checkpoints"],
        json!(0),
        "gemini rows are legal read-only data: {report}"
    );

    // Human output carries the actionable uninstall hint.
    let out = repo.run(&["agent", "doctor"], None);
    assert!(out.status.success(), "doctor: {}", describe(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("libra agent remove gemini"),
        "human output must hint the gemini uninstall channel:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Span coverage — agent.doctor.repair (agent.md §6)
// ---------------------------------------------------------------------------

/// `--repair` emits one `agent.doctor.repair` span per repair attempt with
/// the §6 required fields; detection-only runs emit none; transcript
/// content never reaches the sink. Asserted through the CLI's own tracing
/// stack via `LIBRA_LOG` + `LIBRA_LOG_FILE` (span fields render as the
/// event's span scope), so no in-process fake sink is needed.
#[tokio::test]
async fn repair_span_carries_required_fields_without_transcript() {
    let repo = DoctorRepo::init();
    let marker = "SPAN-FORBIDDEN-TRANSCRIPT-MARKER-42x9";
    let transcript = format!(
        "{{\"type\":\"user\",\"text\":\"{marker}\"}}\n{{\"type\":\"assistant\",\"text\":\"done\"}}\n"
    );
    repo.ingest_checkpoint("sess-span", &transcript);
    let original = repo.checkpoint_rows().await.remove(0);
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![original.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: no repair attempted → no repair span.
    let detect_log = repo.repo.join("doctor-detect.log");
    let out = repo.run_env(
        &["agent", "doctor", "--json"],
        None,
        &[
            ("LIBRA_LOG", "libra=info"),
            ("LIBRA_LOG_FILE", &detect_log.display().to_string()),
        ],
    );
    assert!(out.status.success(), "doctor: {}", describe(&out));
    let detect_captured = std::fs::read_to_string(&detect_log).unwrap_or_default();
    assert!(
        !detect_captured.contains("agent.doctor.repair"),
        "detection-only must not emit repair spans:\n{detect_captured}"
    );

    // Repair: span present with required fields, transcript body absent.
    let repair_log = repo.repo.join("doctor-repair.log");
    let out = repo.run_env(
        &["agent", "doctor", "--repair", "--json"],
        None,
        &[
            ("LIBRA_LOG", "libra=info"),
            ("LIBRA_LOG_FILE", &repair_log.display().to_string()),
        ],
    );
    assert!(out.status.success(), "doctor --repair: {}", describe(&out));
    let captured = std::fs::read_to_string(&repair_log).expect("read doctor span log");
    assert!(
        captured.contains("agent.doctor.repair"),
        "repair span missing:\n{captured}"
    );
    for field in [
        "inconsistency_type=missing_catalog_row",
        "repaired=true",
        "manual_required=false",
    ] {
        assert!(
            captured.contains(field),
            "repair span missing `{field}`:\n{captured}"
        );
    }
    assert!(
        !captured.contains(marker),
        "transcript content must never reach the span sink:\n{captured}"
    );

    // The repair itself worked (row equality on key fields).
    assert_eq!(repo.checkpoint_rows().await, vec![original]);
}

/// Class 3 also repairs rows that EXIST but drifted from the writer's
/// semantics (codex A5 review R5): a transcript blob mis-indexed as a
/// generic `blob` (or with a wrong `o_size`) breaks cloud-sync
/// classification exactly like a missing row. Doctor detects the drift
/// (size verified only via the manifest-declared `byte_len` — payloads
/// are never read) and `--repair` UPDATEs the row in place back to the
/// writer baseline.
#[tokio::test]
async fn class3_drifted_object_index_row_updated_in_place() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-drift-index", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let transcript_oid = sidecar_oid(&repo, &row, "transcript/claude_code.jsonl");
    let oids = [transcript_oid.as_str()];

    let before = repo.object_index_rows(&oids).await;
    assert_eq!(before.len(), 1, "baseline transcript row: {before:?}");
    assert_eq!(before[0].1, "agent_transcript");

    // Drift the row: wrong o_type and wrong size.
    repo.exec_sql(
        "UPDATE object_index SET o_type = 'blob', o_size = 1 WHERE o_id = ?",
        vec![transcript_oid.clone().into()],
    )
    .await;

    // Detection reports the drift, naming the old shape.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("drifted") && detail.contains("was blob/1"),
        "detail must describe the drift: {detail}"
    );

    // Repair restores the writer baseline in place; second runs clean.
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    assert_eq!(
        repo.object_index_rows(&oids).await,
        before,
        "drifted row must be updated back to the writer baseline"
    );
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

// ---------------------------------------------------------------------------
// A0-02 — subagent-scope crash-window-B repair parity
// ---------------------------------------------------------------------------

async fn subagent_parent_link(repo: &DoctorRepo, checkpoint_id: &str) -> Option<String> {
    let conn = repo.db().await;
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT parent_checkpoint_id FROM agent_checkpoint WHERE checkpoint_id = ?",
            vec![checkpoint_id.into()],
        ))
        .await
        .expect("query parent_checkpoint_id")
        .expect("subagent row present");
    row.try_get_by::<Option<String>, _>("parent_checkpoint_id")
        .unwrap()
}

/// A subagent-scope orphan (traces commit present, catalog row deleted) is
/// auto-repaired by `doctor --repair`, which rebuilds a first-class
/// `scope='subagent'` row — including its `parent_checkpoint_id` linkage —
/// from the commit's metadata.json, at parity with the committed class-2 path.
#[tokio::test]
async fn class2_missing_subagent_catalog_row_repaired() {
    let repo = DoctorRepo::init();

    // User commit so the checkpoint's parent_commit is Some(head).
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    assert!(repo.run(&["add", "seed.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "seed"], None).status.success());

    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let session = "sess-subagent-doctor";
    // codex owns the session; Stop writes a committed parent…
    assert!(
        repo.hook(
            "codex",
            "session-start",
            &repo.envelope("SessionStart", session, &transcript),
        )
        .status
        .success(),
        "codex session-start"
    );
    assert!(
        repo.hook(
            "codex",
            "stop",
            &repo.envelope("Stop", session, &transcript)
        )
        .status
        .success(),
        "codex stop"
    );
    // …and a SubagentStop boundary materialises the subagent checkpoint.
    let out = repo.hook(
        "codex",
        "subagent-end",
        &repo.envelope("SubagentStop", session, &transcript),
    );
    assert!(out.status.success(), "subagent-end: {}", describe(&out));

    let rows = repo.checkpoint_rows().await;
    let subagent = rows
        .iter()
        .find(|r| r.scope == "subagent")
        .cloned()
        .expect("a scope='subagent' checkpoint row");
    let parent_link_before = subagent_parent_link(&repo, &subagent.checkpoint_id).await;
    assert!(
        parent_link_before.is_some(),
        "subagent checkpoint must link back to the committed parent"
    );

    // Fabricate window B for the subagent row.
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![subagent.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: reported, not repaired.
    let report = repo.doctor_json(false);
    assert!(
        findings(&report)
            .iter()
            .any(|f| f["checkpoint_id"] == json!(subagent.checkpoint_id)
                && f["inconsistency_type"] == json!("missing_catalog_row")
                && f["repaired"] == json!(false)),
        "subagent window B must be detected without repair: {report}"
    );

    // Repair: a scope='subagent' row comes back with its linkage intact.
    let report = repo.doctor_json(true);
    assert!(
        findings(&report)
            .iter()
            .any(|f| f["checkpoint_id"] == json!(subagent.checkpoint_id)
                && f["repaired"] == json!(true)),
        "subagent row must be auto-repaired: {report}"
    );
    let rows = repo.checkpoint_rows().await;
    let repaired = rows
        .iter()
        .find(|r| r.checkpoint_id == subagent.checkpoint_id)
        .expect("subagent row reinserted");
    assert_eq!(
        repaired.scope, "subagent",
        "repaired row must keep scope='subagent'"
    );
    assert_eq!(
        subagent_parent_link(&repo, &subagent.checkpoint_id).await,
        parent_link_before,
        "parent_checkpoint_id linkage must survive the repair"
    );

    // Idempotent.
    assert_store_clean(&repo.doctor_json(true));
}

// ---------------------------------------------------------------------------
// A0-02 — subagent checkpoint sidecar must not leak the transcript path
// ---------------------------------------------------------------------------

/// Resolve the OID of a named entry among parsed `(mode, name, oid)` tree rows.
fn subagent_entry_oid(entries: &[(String, String, String)], name: &str) -> String {
    entries
        .iter()
        .find(|(_, n, _)| n == name)
        .map(|(_, _, oid)| oid.clone())
        .unwrap_or_else(|| panic!("tree entry '{name}' missing"))
}

/// A0-02 (Codex re-review): a `SubagentStop` whose envelope carries a
/// transcript path must NOT persist that local path in the subagent
/// checkpoint's syncable `events/lifecycle.jsonl` blob — the writer clears
/// `session_ref` on the sidecar event before serialization.
#[tokio::test]
async fn subagent_checkpoint_sidecar_omits_transcript_path() {
    let repo = DoctorRepo::init();
    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let transcript_str = transcript.to_string_lossy().to_string();
    let session = "sess-subagent-sidecar";

    assert!(
        repo.hook(
            "codex",
            "session-start",
            &repo.envelope("SessionStart", session, &transcript),
        )
        .status
        .success(),
        "codex session-start"
    );
    let out = repo.hook(
        "codex",
        "subagent-end",
        &repo.envelope("SubagentStop", session, &transcript),
    );
    assert!(out.status.success(), "subagent-end: {}", describe(&out));

    let subagent = repo
        .checkpoint_rows()
        .await
        .into_iter()
        .find(|r| r.scope == "subagent")
        .expect("a scope='subagent' checkpoint row");

    // Walk root → checkpoint/<id[:2]>/<id[2:]> → events → lifecycle.jsonl.
    let id = &subagent.checkpoint_id;
    let root = tree_entries(&repo, &subagent.tree_oid);
    let checkpoint = tree_entries(&repo, &subagent_entry_oid(&root, "checkpoint"));
    let prefix = tree_entries(&repo, &subagent_entry_oid(&checkpoint, &id[..2]));
    let inner = tree_entries(&repo, &subagent_entry_oid(&prefix, &id[2..]));
    let events = tree_entries(&repo, &subagent_entry_oid(&inner, "events"));
    let blob_oid = subagent_entry_oid(&events, "lifecycle.jsonl");
    let (blob_type, blob) = read_loose(&repo, &blob_oid);
    assert_eq!(blob_type, "blob");
    let content = String::from_utf8_lossy(&blob);

    assert!(
        !content.contains(&transcript_str),
        "events/lifecycle.jsonl must not leak the transcript path '{transcript_str}':\n{content}"
    );
    assert!(
        !content.contains("session_ref"),
        "the subagent sidecar event must not carry session_ref:\n{content}"
    );
}

/// A0-03: a checkpoint operation on an inconsistent store (a catalog row whose
/// `parent_commit` points at an object missing from the store) fails closed
/// with the stable `LBR-AGENT-009` (`AgentCheckpointStoreInconsistent`) code,
/// not a bare fatal.
#[tokio::test]
async fn checkpoint_store_inconsistent_emits_lbr_agent_009() {
    let repo = DoctorRepo::init();

    // A user commit so the checkpoint records a real parent_commit.
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    assert!(repo.run(&["add", "seed.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "seed"], None).status.success());

    repo.ingest_checkpoint("sess-inconsistent", TRANSCRIPT);
    let cp = repo.checkpoint_rows().await.remove(0);

    // Corrupt the catalog: point parent_commit at a non-existent object.
    let bogus = "b".repeat(40);
    repo.exec_sql(
        "UPDATE agent_checkpoint SET parent_commit = ? WHERE checkpoint_id = ?",
        vec![bogus.into(), cp.checkpoint_id.clone().into()],
    )
    .await;

    let out = repo.run(
        &[
            "agent",
            "checkpoint",
            "rewind",
            &cp.checkpoint_id,
            "--dry-run",
        ],
        None,
    );
    assert!(
        !out.status.success(),
        "rewind on a corrupted store must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-009"),
        "an inconsistent store must carry LBR-AGENT-009: {stderr}"
    );
}

/// A0-03 (Codex re-review): a rewind whose parent tree's ROOT is present but a
/// nested subtree object is missing must fail closed with `LBR-AGENT-009`, not
/// panic through `Tree::load`. Regression for the recursive tree-walk path.
#[tokio::test]
async fn checkpoint_store_missing_nested_tree_emits_lbr_agent_009() {
    let repo = DoctorRepo::init();

    // A commit whose tree contains a subdirectory (a nested subtree object).
    std::fs::create_dir_all(repo.repo.join("nested")).expect("mkdir nested");
    std::fs::write(repo.repo.join("nested").join("f.txt"), "x\n").expect("write nested file");
    assert!(repo.run(&["add", "nested/f.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "nested"], None).status.success());

    repo.ingest_checkpoint("sess-nested", TRANSCRIPT);
    let cp = repo.checkpoint_rows().await.remove(0);
    let parent_commit = cp
        .parent_commit
        .clone()
        .expect("checkpoint records a parent_commit");

    // Resolve the root tree from the commit object, find the "nested" subtree,
    // and delete that object — leaving the root tree readable.
    let (_, commit_body) = read_loose(&repo, &parent_commit);
    let commit_text = String::from_utf8_lossy(&commit_body);
    let root_tree = commit_text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("tree "))
        .expect("commit starts with a tree line")
        .trim()
        .to_string();
    let subtree_oid = subagent_entry_oid(&tree_entries(&repo, &root_tree), "nested");
    let obj = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&subtree_oid[..2])
        .join(&subtree_oid[2..]);
    std::fs::remove_file(&obj).expect("delete nested subtree object");

    let out = repo.run(
        &[
            "agent",
            "checkpoint",
            "rewind",
            &cp.checkpoint_id,
            "--dry-run",
        ],
        None,
    );
    assert!(
        !out.status.success(),
        "rewind with a missing nested tree must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-009"),
        "a missing nested tree must carry LBR-AGENT-009: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "rewind must not panic on a missing subtree: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// A0-06 — findings-object repair (review/investigate objectized findings)
// ---------------------------------------------------------------------------

/// A0-06: a review run's objectized findings blob that goes missing while
/// `findings.md` remains is detected as `missing_findings_object` and
/// auto-repaired by `doctor --repair` (content-addressed rewrite from
/// findings.md), idempotently.
#[tokio::test]
async fn findings_object_repair() {
    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    store
        .create_run(
            "findings-run",
            &["codex".to_string()],
            "sha",
            "HEAD~1..HEAD",
        )
        .expect("create run");
    store
        .write_findings("findings-run", "review finding body line\n")
        .expect("write findings");
    store
        .finalize_run(
            "findings-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");

    let manifest = store
        .load_manifest("findings-run")
        .expect("load manifest")
        .expect("manifest");
    let findings_oid = manifest
        .findings_oid
        .expect("A0-06 populated findings_oid at finalize");
    let obj_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    assert!(
        obj_path.exists(),
        "findings object must be written at finalize"
    );

    // Fabricate the missing-object case (findings.md stays on disk).
    std::fs::remove_file(&obj_path).expect("delete findings object");

    // Detection-only: reported, auto-repairable, not yet repaired.
    let report = repo.doctor_json(false);
    let found = report["findings_store"]["findings"]
        .as_array()
        .expect("findings_store.findings array");
    assert!(
        found
            .iter()
            .any(|f| f["inconsistency_type"] == "missing_findings_object"
                && f["run_id"] == "findings-run"
                && f["repaired"] == false
                && f["manual_required"] == false),
        "missing findings object must be detected auto-repairable: {report}"
    );
    assert!(
        !obj_path.exists(),
        "detection-only must not rewrite the object"
    );

    // Repair: object rewritten from findings.md.
    let report = repo.doctor_json(true);
    assert!(
        report["findings_store"]["repaired"].as_u64().unwrap_or(0) >= 1,
        "--repair must rewrite the findings object: {report}"
    );
    assert!(
        obj_path.exists(),
        "the findings object must be restored by --repair"
    );

    // Idempotent: a second run finds no missing_findings_object.
    let report = repo.doctor_json(true);
    assert!(
        report["findings_store"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["inconsistency_type"] != "missing_findings_object"),
        "second repair run must be clean: {report}"
    );
}

/// A0-06 (codex P1-1): a findings blob already indexed under a different
/// `agent_*` tag (e.g. `agent_transcript`, because identical bytes were first
/// seen as a transcript) with the correct size must NOT be flagged as
/// `missing_findings_object_index` drift — otherwise doctor and the index
/// writer (which refuses to retag an `agent_*` row) fight a tag-war forever.
#[tokio::test]
async fn findings_object_index_tolerates_agent_transcript_tag() {
    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    let findings_body = "review finding body line\n";
    store
        .create_run("tag-run", &["codex".to_string()], "sha", "HEAD~1..HEAD")
        .expect("create run");
    store
        .write_findings("tag-run", findings_body)
        .expect("write findings");
    store
        .finalize_run(
            "tag-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");
    let manifest = store
        .load_manifest("tag-run")
        .expect("load manifest")
        .expect("manifest");
    let findings_oid = manifest.findings_oid.expect("findings_oid populated");

    // Resolve doctor's repo_id and pre-index the findings blob under the
    // agent_transcript tag with the correct payload size.
    let repo_id = {
        let conn = repo.db().await;
        let backend = conn.get_database_backend();
        conn.query_one(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key='libra.repoid'",
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get_by::<String, _>("value").ok())
        .unwrap_or_else(|| "unknown-repo".to_string())
    };
    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id = ?",
        vec![findings_oid.clone().into()],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
         VALUES (?, ?, ?, ?, ?, 0)",
        vec![
            findings_oid.clone().into(),
            "agent_transcript".into(),
            (findings_body.len() as i64).into(),
            repo_id.into(),
            0i64.into(),
        ],
    )
    .await;

    // A --repair run must NOT flag the size-matching agent_transcript row.
    let report = repo.doctor_json(true);
    let findings = report["findings_store"]["findings"]
        .as_array()
        .expect("findings_store.findings array");
    assert!(
        findings
            .iter()
            .all(|f| f["inconsistency_type"] != "missing_findings_object_index"),
        "a size-matching agent_* index row must not be flagged as findings drift: {report}"
    );
    // The tag is preserved — no tag-war rewrite.
    let rows = repo.object_index_rows(&[&findings_oid]).await;
    assert!(
        rows.iter()
            .any(|(_, o_type, _, _)| o_type == "agent_transcript"),
        "the pre-existing agent_transcript tag must be left intact: {rows:?}"
    );
}
