//! AG-20 E4-libra checkpoint export writer tests (plan.md Task A5).
//!
//! Drives the hook-ingest writer in-process
//! (`libra::internal::ai::hooks::runtime::ingest_agent_traces_payload`,
//! exported `pub` for tests — not a stable API) against a fresh
//! production-shaped SQLite database plus an isolated objects directory,
//! then walks the resulting `refs/libra/traces` checkpoint tree straight
//! from the on-disk Git objects. Covered contracts:
//!
//! - the writer emits the full E4-libra tree (`metadata.json`,
//!   `manifest.json`, `events/lifecycle.jsonl`,
//!   `transcript/<agent_kind>.jsonl`, `redaction_report.json`,
//!   `content_hash.txt`) with exact names;
//! - `manifest.json` roles/OIDs/byte lengths match the actual blobs;
//! - `content_hash.txt` is `sha256:<64-hex>` and recomputes over the
//!   manifest-declared coverage; the reader helper tolerates legacy bare
//!   hex;
//! - E5 chunking: small transcripts stay single-file; transcripts above
//!   the (test-overridden) threshold split into ordered, line-safe
//!   `.jsonl.%03d` parts declared by the manifest;
//! - stage (d) catalog idempotency: probe-by-`traces_commit` +
//!   `ON CONFLICT(checkpoint_id) DO NOTHING` keep crash retries at exactly
//!   one row;
//! - window A/B in-flight markers: written before the blobs, cleared after
//!   the catalog INSERT, and expired markers drop out of the live listing.
//!
//! Env mutation (`LIBRA_TEST_HOME`, `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD`)
//! makes the ingest-driving tests `#[serial]`.

use std::path::{Path, PathBuf};

use libra::internal::ai::{
    history::{self, TracesInflightMarker, checkpoint_content_hash, parse_content_hash},
    hooks::{
        LifecycleEventKind, ProviderHookCommand, claude_provider,
        runtime::{
            AgentCheckpointRow, SubagentCheckpointRow, ingest_agent_traces_payload,
            insert_agent_checkpoint_row_idempotent, insert_subagent_checkpoint_row_idempotent,
        },
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One isolated ingest target: a production-shaped `libra.db` (created via
/// the same `create_database` path `libra init` uses) sharing a tempdir
/// with the `objects/` store, plus a fake `$HOME` carrying the provider
/// transcript under `~/.claude/`.
struct ExportRepo {
    _dir: TempDir,
    home: TempDir,
    repo_path: PathBuf,
    conn: DatabaseConnection,
    transcript_path: PathBuf,
}

impl ExportRepo {
    async fn init(transcript: &[u8]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_path = dir.path().to_path_buf();
        let db_path = repo_path.join("libra.db");
        let conn = libra::internal::db::create_database(&db_path.display().to_string())
            .await
            .expect("create fresh libra database");

        let home = tempfile::tempdir().expect("fake home tempdir");
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).expect("create fake ~/.claude");
        let transcript_path = claude_dir.join("session-transcript.jsonl");
        std::fs::write(&transcript_path, transcript).expect("write provider transcript");

        Self {
            _dir: dir,
            home,
            repo_path,
            conn,
            transcript_path,
        }
    }

    fn envelope(&self, hook_event_name: &str, session_id: &str, extra: Value) -> Vec<u8> {
        let mut base = json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": "/tmp/repo",
            "transcript_path": self.transcript_path.display().to_string(),
        });
        if let (Value::Object(extra_map), Some(base_map)) = (extra, base.as_object_mut()) {
            for (key, value) in extra_map {
                base_map.insert(key, value);
            }
        }
        serde_json::to_vec(&base).expect("serialize envelope")
    }

    async fn ingest(&self, payload: &[u8], command: ProviderHookCommand, kind: LifecycleEventKind) {
        ingest_agent_traces_payload(
            payload,
            command,
            kind,
            claude_provider(),
            &self.conn,
            Some(&self.repo_path),
        )
        .await
        .expect("ingest succeeds");
    }

    /// Run SessionStart + SessionEnd for `session_id` under a fake HOME so
    /// the adapter's transcript trust check passes, returning the single
    /// resulting `agent_checkpoint` row.
    async fn ingest_session(&self, session_id: &str, end_extra: Value) -> CheckpointRow {
        let prior_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: test-only env mutation, restored below; the test is
        // #[serial] so no sibling reads the variable concurrently.
        unsafe {
            std::env::set_var("LIBRA_TEST_HOME", self.home.path());
        }
        self.ingest(
            &self.envelope("SessionStart", session_id, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
        )
        .await;
        self.ingest(
            &self.envelope("SessionEnd", session_id, end_extra),
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
        )
        .await;
        unsafe {
            match prior_home {
                Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }
        self.checkpoint_row(session_id).await
    }

    async fn checkpoint_row(&self, provider_session_id: &str) -> CheckpointRow {
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, session_id, tree_oid, metadata_blob_oid, traces_commit \
                 FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1) \
                 LIMIT 1",
                [provider_session_id.into()],
            ))
            .await
            .expect("query agent_checkpoint")
            .expect("checkpoint row exists");
        CheckpointRow {
            checkpoint_id: row.try_get_by("checkpoint_id").unwrap(),
            session_id: row.try_get_by("session_id").unwrap(),
            tree_oid: row.try_get_by("tree_oid").unwrap(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap(),
            traces_commit: row.try_get_by("traces_commit").unwrap(),
        }
    }

    /// Resolve the inner checkpoint tree
    /// (`checkpoint/<id[:2]>/<id[2:]>/`) entries for a catalog row.
    fn inner_tree(&self, row: &CheckpointRow) -> Vec<TreeEntry> {
        let root = read_tree(&self.repo_path, &row.tree_oid);
        let checkpoint = subtree(&self.repo_path, &root, "checkpoint");
        let prefix = subtree(&self.repo_path, &checkpoint, &row.checkpoint_id[..2]);
        subtree(&self.repo_path, &prefix, &row.checkpoint_id[2..])
    }

    fn blob(&self, oid: &str) -> Vec<u8> {
        let (object_type, body) = read_object(&self.repo_path, oid);
        assert_eq!(object_type, "blob", "object {oid} must be a blob");
        body
    }
}

#[derive(Debug, Clone)]
struct CheckpointRow {
    checkpoint_id: String,
    session_id: String,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
}

#[derive(Debug, Clone)]
struct TreeEntry {
    mode: String,
    name: String,
    oid: String,
}

/// Read + zlib-decode one loose object, returning `(type, body)`.
fn read_object(repo_path: &Path, oid: &str) -> (String, Vec<u8>) {
    let object_path = repo_path.join("objects").join(&oid[..2]).join(&oid[2..]);
    let raw = std::fs::read(&object_path)
        .unwrap_or_else(|e| panic!("read object {oid} at {object_path:?}: {e}"));
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

/// Parse a (SHA-1) tree object's entries.
fn read_tree(repo_path: &Path, oid: &str) -> Vec<TreeEntry> {
    let (object_type, body) = read_object(repo_path, oid);
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
            .unwrap()
            .to_string();
        let name_start = space + 1;
        let null = name_start
            + body[name_start..]
                .iter()
                .position(|&b| b == 0)
                .expect("name terminator");
        let name = std::str::from_utf8(&body[name_start..null])
            .unwrap()
            .to_string();
        let hash_start = null + 1;
        let oid = hex::encode(&body[hash_start..hash_start + 20]);
        entries.push(TreeEntry { mode, name, oid });
        cursor = hash_start + 20;
    }
    entries
}

fn subtree(repo_path: &Path, entries: &[TreeEntry], name: &str) -> Vec<TreeEntry> {
    let entry = entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("tree entry '{name}' missing from {entries:?}"));
    assert_eq!(entry.mode, "40000", "'{name}' must be a tree entry");
    read_tree(repo_path, &entry.oid)
}

fn entry<'a>(entries: &'a [TreeEntry], name: &str) -> &'a TreeEntry {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("tree entry '{name}' missing from {entries:?}"))
}

/// Resolve a manifest-relative path (e.g. `events/lifecycle.jsonl`) to the
/// blob OID recorded in the checkpoint tree.
fn resolve_path(repo: &ExportRepo, inner: &[TreeEntry], path: &str) -> String {
    let mut entries = inner.to_vec();
    let segments: Vec<&str> = path.split('/').collect();
    for segment in &segments[..segments.len() - 1] {
        entries = subtree(&repo.repo_path, &entries, segment);
    }
    entry(&entries, segments[segments.len() - 1]).oid.clone()
}

const SMALL_TRANSCRIPT: &[u8] =
    b"{\"role\":\"user\",\"text\":\"kick off\"}\n{\"role\":\"assistant\",\"text\":\"done marker-e4\"}\n";

// ---------------------------------------------------------------------------
// E4-libra tree shape
// ---------------------------------------------------------------------------

/// The writer emits exactly the six E4-libra entries with their exact
/// names; the transcript file is `<agent_kind>.jsonl` (snake_case db tag +
/// extension — the rename from v1's extension-less `transcript/<provider>`),
/// the events file is `events/lifecycle.jsonl`, and metadata.json carries
/// schema_version 2 plus the `model` field.
#[tokio::test]
#[serial]
async fn writer_emits_all_six_e4_libra_entries() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo.ingest_session("sess-e4-shape", json!({})).await;

    let inner = repo.inner_tree(&row);
    let mut names: Vec<&str> = inner.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "content_hash.txt",
            "events",
            "manifest.json",
            "metadata.json",
            "redaction_report.json",
            "transcript",
        ],
        "inner checkpoint tree must contain exactly the six E4-libra entries"
    );

    // events/ holds exactly lifecycle.jsonl.
    let events = subtree(&repo.repo_path, &inner, "events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "lifecycle.jsonl");

    // transcript/ holds exactly <agent_kind>.jsonl (small input: no chunks).
    let transcript = subtree(&repo.repo_path, &inner, "transcript");
    assert_eq!(transcript.len(), 1);
    assert_eq!(
        transcript[0].name, "claude_code.jsonl",
        "transcript file must be the snake_case agent kind WITH the .jsonl extension"
    );
    // Small transcript captured verbatim (no secrets → redaction identity).
    assert_eq!(repo.blob(&transcript[0].oid), SMALL_TRANSCRIPT);

    // metadata.json: schema v2, model tolerance, catalog pointer agreement.
    let metadata_oid = entry(&inner, "metadata.json").oid.clone();
    assert_eq!(
        metadata_oid, row.metadata_blob_oid,
        "catalog metadata_blob_oid must point at the tree's metadata.json"
    );
    let metadata: Value = serde_json::from_slice(&repo.blob(&metadata_oid)).unwrap();
    assert_eq!(metadata["schema_version"], json!(2));
    assert_eq!(metadata["checkpoint_id"], json!(row.checkpoint_id));
    assert_eq!(metadata["session_id"], json!(row.session_id));
    assert_eq!(metadata["agent_kind"], json!("claude_code"));
    assert_eq!(
        metadata["model"],
        json!("unknown"),
        "an envelope without model must record the literal 'unknown'"
    );
    assert!(
        metadata["redaction_report"].is_object(),
        "v1 field redaction_report must survive in v2 (additive schema)"
    );

    // events/lifecycle.jsonl: one canonical E3 line for the SessionEnd.
    let events_bytes = repo.blob(&events[0].oid);
    let text = String::from_utf8(events_bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "one triggering event → one JSONL line");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["schema_version"], json!(1));
    assert_eq!(line["kind"], json!("session_end"));
    assert_eq!(line["agent_kind"], json!("claude_code"));
    assert_eq!(line["session_id"], json!(row.session_id));
    assert_eq!(line["provider_session_id"], json!("sess-e4-shape"));
    assert_eq!(line["partial"], json!(false));
    assert_eq!(line["provenance"]["hook_event_name"], json!("SessionEnd"));
    assert!(
        uuid::Uuid::parse_str(line["event_id"].as_str().unwrap()).is_ok(),
        "event_id must be a UUID"
    );

    // redaction_report.json parses as a report object.
    let report: Value =
        serde_json::from_slice(&repo.blob(&entry(&inner, "redaction_report.json").oid)).unwrap();
    assert!(report.is_object());
    assert!(report.get("matches").is_some());
}

/// A model carried by the triggering event lands in metadata.json instead
/// of "unknown".
#[tokio::test]
#[serial]
async fn metadata_model_field_prefers_event_model() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo
        .ingest_session("sess-e4-model", json!({"model": "claude-sonnet-4-5"}))
        .await;
    let metadata: Value = serde_json::from_slice(&repo.blob(&row.metadata_blob_oid)).unwrap();
    assert_eq!(metadata["model"], json!("claude-sonnet-4-5"));
}

// ---------------------------------------------------------------------------
// manifest.json ↔ blobs agreement
// ---------------------------------------------------------------------------

/// Every manifest role's `path` resolves in the tree to a blob whose OID
/// and byte length match the manifest declaration.
#[tokio::test]
#[serial]
async fn manifest_roles_oids_and_lengths_match_actual_blobs() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo.ingest_session("sess-e4-manifest", json!({})).await;
    let inner = repo.inner_tree(&row);

    let manifest: Value =
        serde_json::from_slice(&repo.blob(&entry(&inner, "manifest.json").oid)).unwrap();
    assert_eq!(manifest["schema_version"], json!(1));
    assert_eq!(manifest["checkpoint_id"], json!(row.checkpoint_id));
    assert_eq!(manifest["content_hash"]["algorithm"], json!("sha256"));
    assert_eq!(
        manifest["content_hash"]["coverage"],
        json!([
            "metadata",
            "lifecycle_events",
            "transcript",
            "redaction_report"
        ]),
        "the manifest must self-describe the content-hash coverage order"
    );

    let entries = manifest["entries"].as_object().expect("entries object");
    let mut roles: Vec<&str> = entries.keys().map(String::as_str).collect();
    roles.sort_unstable();
    assert_eq!(
        roles,
        vec![
            "content_hash",
            "lifecycle_events",
            "metadata",
            "redaction_report",
            "transcript",
        ]
    );

    for (role, declared) in entries {
        let path = declared["path"].as_str().expect("entry path");
        let oid = declared["oid"].as_str().expect("entry oid (single-blob)");
        let byte_len = declared["byte_len"].as_u64().expect("entry byte_len");
        assert_eq!(declared["compression"], json!("none"));
        assert!(
            declared["schema_version"].is_u64(),
            "role {role} must declare a schema_version"
        );
        let tree_oid = resolve_path(&repo, &inner, path);
        assert_eq!(tree_oid, oid, "role {role}: manifest oid vs tree oid");
        let blob = repo.blob(oid);
        assert_eq!(
            blob.len() as u64,
            byte_len,
            "role {role}: manifest byte_len vs actual blob"
        );
    }

    // Redaction states per role.
    assert_eq!(entries["transcript"]["redaction"], json!("redacted"));
    assert_eq!(entries["lifecycle_events"]["redaction"], json!("redacted"));
    assert_eq!(entries["metadata"]["redaction"], json!("redacted"));
    assert_eq!(entries["redaction_report"]["redaction"], json!("report"));
    assert_eq!(entries["content_hash"]["redaction"], json!("none"));
}

// ---------------------------------------------------------------------------
// content_hash.txt
// ---------------------------------------------------------------------------

/// `content_hash.txt` is `sha256:` + 64 lowercase hex with no trailing
/// newline, and recomputes exactly from the coverage roles' bytes in
/// manifest-declared order. The reader helper also accepts legacy bare hex.
#[tokio::test]
#[serial]
async fn content_hash_format_and_recompute() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo.ingest_session("sess-e4-hash", json!({})).await;
    let inner = repo.inner_tree(&row);

    let hash_bytes = repo.blob(&entry(&inner, "content_hash.txt").oid);
    let hash_text = String::from_utf8(hash_bytes).unwrap();
    assert!(
        hash_text.starts_with("sha256:"),
        "writer output must carry the sha256: prefix: {hash_text}"
    );
    assert!(
        !hash_text.ends_with('\n'),
        "content_hash.txt must not carry a trailing newline"
    );
    let digest = parse_content_hash(&hash_text).expect("writer hash must parse");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));

    // Recompute over the manifest coverage in declared order.
    let manifest: Value =
        serde_json::from_slice(&repo.blob(&entry(&inner, "manifest.json").oid)).unwrap();
    let coverage: Vec<String> = manifest["content_hash"]["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|role| role.as_str().unwrap().to_string())
        .collect();
    let mut sections: Vec<Vec<u8>> = Vec::new();
    for role in &coverage {
        let declared = &manifest["entries"][role];
        let path = declared["path"].as_str().unwrap();
        sections.push(repo.blob(&resolve_path(&repo, &inner, path)));
    }
    let section_slices: Vec<&[u8]> = sections.iter().map(Vec::as_slice).collect();
    let recomputed = checkpoint_content_hash(&section_slices);
    assert_eq!(
        recomputed, hash_text,
        "content hash must recompute from the coverage bytes"
    );

    // Reader tolerance stub: legacy bare hex parses to the same digest
    // (writer never emits it, readers must accept it).
    assert_eq!(parse_content_hash(&digest), Some(digest.clone()));
}

// ---------------------------------------------------------------------------
// E5 chunking
// ---------------------------------------------------------------------------

/// Transcripts above the threshold split at line boundaries into ordered
/// `.jsonl.%03d` parts; the manifest declares the parts (in order) under
/// the logical transcript role; the parts reassemble byte-identically; and
/// the content hash covers the logical (reassembled) stream.
#[tokio::test]
#[serial]
async fn chunking_large_transcript_splits_line_safe() {
    // ~40 bytes per line × 40 lines ≈ 1.6 KiB, threshold 256 → ≥ 6 chunks.
    let mut transcript = Vec::new();
    for index in 0..40 {
        transcript.extend_from_slice(
            format!("{{\"turn\":{index:04},\"text\":\"chunk me\"}}\n").as_bytes(),
        );
    }
    let threshold = 256usize;

    let repo = ExportRepo::init(&transcript).await;
    let prior = std::env::var_os("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD");
    // SAFETY: test-only env mutation under #[serial], restored below.
    unsafe {
        std::env::set_var(
            "LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD",
            threshold.to_string(),
        );
    }
    let row = repo.ingest_session("sess-e5-chunks", json!({})).await;
    unsafe {
        match prior {
            Some(value) => std::env::set_var("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD", value),
            None => std::env::remove_var("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD"),
        }
    }

    let inner = repo.inner_tree(&row);
    let transcript_tree = subtree(&repo.repo_path, &inner, "transcript");
    assert!(
        transcript_tree.len() > 1,
        "large transcript must split into multiple parts: {transcript_tree:?}"
    );
    assert!(
        transcript_tree
            .iter()
            .all(|entry| entry.name != "claude_code.jsonl"),
        "chunked layout must not also carry the unchunked file"
    );

    // Manifest declares the logical role with ordered parts and no
    // single-blob oid.
    let manifest: Value =
        serde_json::from_slice(&repo.blob(&entry(&inner, "manifest.json").oid)).unwrap();
    let declared = &manifest["entries"]["transcript"];
    assert_eq!(declared["path"], json!("transcript/claude_code.jsonl"));
    assert_eq!(declared["chunked"], json!(true));
    assert!(
        declared.get("oid").is_none(),
        "a chunked transcript has no single blob oid"
    );
    let parts = declared["parts"].as_array().expect("ordered parts");
    assert_eq!(parts.len(), transcript_tree.len());

    // Parts are numbered .001, .002, … in manifest order; each part is
    // within the threshold, ends on a line boundary, and the declared
    // byte_len matches the blob.
    let mut reassembled = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let expected_name = format!("claude_code.jsonl.{:03}", index + 1);
        let path = part["path"].as_str().unwrap();
        assert_eq!(path, format!("transcript/{expected_name}"));
        let oid = part["oid"].as_str().unwrap();
        assert_eq!(
            entry(&transcript_tree, &expected_name).oid,
            oid,
            "part {expected_name}: manifest oid vs tree oid"
        );
        let bytes = repo.blob(oid);
        assert_eq!(bytes.len() as u64, part["byte_len"].as_u64().unwrap());
        assert!(
            bytes.len() <= threshold,
            "part {expected_name} exceeds the threshold"
        );
        assert!(
            bytes.ends_with(b"\n"),
            "part {expected_name} must end at a line boundary"
        );
        reassembled.extend_from_slice(&bytes);
    }
    // Line-boundary property + byte identity: the concatenation equals the
    // source transcript (no secrets → redaction is the identity).
    assert_eq!(reassembled, transcript);
    assert_eq!(
        declared["byte_len"].as_u64().unwrap(),
        transcript.len() as u64,
        "logical byte_len must be the total across parts"
    );

    // content_hash covers the logical stream, so it must recompute from
    // the reassembled bytes.
    let metadata_bytes = repo.blob(&entry(&inner, "metadata.json").oid);
    let events_bytes = repo.blob(&resolve_path(&repo, &inner, "events/lifecycle.jsonl"));
    let report_bytes = repo.blob(&entry(&inner, "redaction_report.json").oid);
    let hash_text = String::from_utf8(repo.blob(&entry(&inner, "content_hash.txt").oid)).unwrap();
    assert_eq!(
        checkpoint_content_hash(&[&metadata_bytes, &events_bytes, &reassembled, &report_bytes]),
        hash_text,
        "content hash must be invariant under chunking (logical stream)"
    );
}

/// A single line larger than the threshold is a hard error (E5), asserted
/// at the chunker contract level so the test does not need to construct a
/// broken end-to-end ingest.
#[test]
fn chunking_oversize_single_line_is_hard_error() {
    let one_line = vec![b'y'; 512];
    let err = history::chunk_transcript_line_safe(&one_line, 256).unwrap_err();
    assert!(
        err.to_string().contains("refusing to split mid-line"),
        "error must name the E5 mid-line refusal: {err}"
    );
}

// ---------------------------------------------------------------------------
// Idempotent catalog write (stage d)
// ---------------------------------------------------------------------------

/// The stage-(d) helper is crash-retry safe: a second attempt for the same
/// `traces_commit` (fresh checkpoint id — the retry scenario) and a replay
/// of the exact same row (the `ON CONFLICT(checkpoint_id)` backstop) both
/// leave exactly one row.
#[tokio::test]
#[serial]
async fn catalog_insert_is_idempotent_across_crash_retries() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo.ingest_session("sess-idem", json!({})).await;

    let count = |conn: &DatabaseConnection, commit: String| {
        let conn = conn.clone();
        async move {
            let backend = conn.get_database_backend();
            let row = conn
                .query_one(Statement::from_sql_and_values(
                    backend,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE traces_commit = ?",
                    [commit.into()],
                ))
                .await
                .expect("count query")
                .expect("count row");
            row.try_get_by::<i64, _>("n").unwrap()
        }
    };
    assert_eq!(count(&repo.conn, row.traces_commit.clone()).await, 1);

    // The probe sees the ingest's row.
    let probed = history::agent_checkpoint_id_for_traces_commit(&repo.conn, &row.traces_commit)
        .await
        .expect("probe");
    assert_eq!(probed.as_deref(), Some(row.checkpoint_id.as_str()));

    // Crash-retry: same commit, fresh checkpoint id → deduped, no insert.
    let retry_id = uuid::Uuid::new_v4().to_string();
    let inserted = insert_agent_checkpoint_row_idempotent(
        &repo.conn,
        &AgentCheckpointRow {
            checkpoint_id: &retry_id,
            session_id: &row.session_id,
            parent_commit: None,
            tree_oid: &row.tree_oid,
            metadata_blob_oid: &row.metadata_blob_oid,
            traces_commit: &row.traces_commit,
            created_at: 1,
        },
    )
    .await
    .expect("idempotent insert");
    assert!(
        !inserted,
        "retry for an already-cataloged commit must dedupe"
    );
    assert_eq!(count(&repo.conn, row.traces_commit.clone()).await, 1);

    // Backstop: replaying the winning row itself is also a no-op.
    let replayed = insert_agent_checkpoint_row_idempotent(
        &repo.conn,
        &AgentCheckpointRow {
            checkpoint_id: &row.checkpoint_id,
            session_id: &row.session_id,
            parent_commit: None,
            tree_oid: &row.tree_oid,
            metadata_blob_oid: &row.metadata_blob_oid,
            traces_commit: &row.traces_commit,
            created_at: 1,
        },
    )
    .await
    .expect("replay insert");
    assert!(!replayed);
    assert_eq!(count(&repo.conn, row.traces_commit).await, 1);

    // An unknown commit probes to None and inserts fresh (true).
    let fresh_commit = "f".repeat(40);
    assert_eq!(
        history::agent_checkpoint_id_for_traces_commit(&repo.conn, &fresh_commit)
            .await
            .unwrap(),
        None
    );
    let fresh_id = uuid::Uuid::new_v4().to_string();
    let inserted_fresh = insert_agent_checkpoint_row_idempotent(
        &repo.conn,
        &AgentCheckpointRow {
            checkpoint_id: &fresh_id,
            session_id: &row.session_id,
            parent_commit: None,
            tree_oid: &row.tree_oid,
            metadata_blob_oid: &row.metadata_blob_oid,
            traces_commit: &fresh_commit,
            created_at: 2,
        },
    )
    .await
    .expect("fresh insert");
    assert!(inserted_fresh);
}

/// A0-02 subagent_scope: a `scope='subagent'` catalog row persists its
/// subagent-specific linkage columns (`parent_checkpoint_id`,
/// `subagent_session_id`, `tool_use_id`, `description`) that the committed
/// path leaves NULL, dedupes idempotently by `traces_commit`, and coexists
/// with the parent `committed` row so the two stay distinguishable by scope.
#[tokio::test]
#[serial]
async fn subagent_scope_row_persists_linkage_and_is_idempotent() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    // Parent committed checkpoint the subagent links back to.
    let parent = repo.ingest_session("sess-subagent", json!({})).await;

    let sub_commit = "a".repeat(40);
    let sub_id = uuid::Uuid::new_v4().to_string();
    let inserted = insert_subagent_checkpoint_row_idempotent(
        &repo.conn,
        &SubagentCheckpointRow {
            checkpoint_id: &sub_id,
            session_id: &parent.session_id,
            parent_commit: Some(&parent.traces_commit),
            parent_checkpoint_id: Some(&parent.checkpoint_id),
            subagent_session_id: Some("sub-run-123"),
            tool_use_id: Some("Task"),
            description: Some("subagent end via Task"),
            tree_oid: &parent.tree_oid,
            metadata_blob_oid: &parent.metadata_blob_oid,
            traces_commit: &sub_commit,
            created_at: 7,
        },
    )
    .await
    .expect("insert subagent row");
    assert!(inserted, "a fresh subagent row must insert");

    // The row persisted with scope='subagent' and all four linkage columns.
    let row = repo
        .conn
        .query_one(Statement::from_sql_and_values(
            repo.conn.get_database_backend(),
            "SELECT scope, parent_checkpoint_id, subagent_session_id, tool_use_id, description \
             FROM agent_checkpoint WHERE checkpoint_id = ?",
            [sub_id.clone().into()],
        ))
        .await
        .expect("query subagent row")
        .expect("subagent row present");
    assert_eq!(row.try_get_by::<String, _>("scope").unwrap(), "subagent");
    assert_eq!(
        row.try_get_by::<String, _>("parent_checkpoint_id").unwrap(),
        parent.checkpoint_id
    );
    assert_eq!(
        row.try_get_by::<String, _>("subagent_session_id").unwrap(),
        "sub-run-123"
    );
    assert_eq!(row.try_get_by::<String, _>("tool_use_id").unwrap(), "Task");
    assert_eq!(
        row.try_get_by::<String, _>("description").unwrap(),
        "subagent end via Task"
    );

    // Crash-retry with a fresh id but the same traces_commit → deduped.
    let retry_id = uuid::Uuid::new_v4().to_string();
    let replayed = insert_subagent_checkpoint_row_idempotent(
        &repo.conn,
        &SubagentCheckpointRow {
            checkpoint_id: &retry_id,
            session_id: &parent.session_id,
            parent_commit: Some(&parent.traces_commit),
            parent_checkpoint_id: Some(&parent.checkpoint_id),
            subagent_session_id: Some("sub-run-123"),
            tool_use_id: Some("Task"),
            description: Some("subagent end via Task"),
            tree_oid: &parent.tree_oid,
            metadata_blob_oid: &parent.metadata_blob_oid,
            traces_commit: &sub_commit,
            created_at: 8,
        },
    )
    .await
    .expect("replay subagent insert");
    assert!(!replayed, "a subagent retry on the same commit must dedupe");

    // Committed parent and subagent child coexist, distinguishable by scope.
    let scopes = repo
        .conn
        .query_all(Statement::from_sql_and_values(
            repo.conn.get_database_backend(),
            "SELECT scope FROM agent_checkpoint WHERE session_id = ? ORDER BY scope",
            [parent.session_id.clone().into()],
        ))
        .await
        .expect("query scopes")
        .into_iter()
        .map(|r| r.try_get_by::<String, _>("scope").unwrap())
        .collect::<Vec<_>>();
    assert!(
        scopes.contains(&"committed".to_string()) && scopes.contains(&"subagent".to_string()),
        "both a committed and a subagent scope row must coexist, got {scopes:?}"
    );
}

// ---------------------------------------------------------------------------
// Window A/B in-flight markers
// ---------------------------------------------------------------------------

/// Marker API lifecycle: write → live-listed; clear → gone; expired
/// markers (TTL elapsed) drop out of the live listing. This is the API the
/// prune side consumes.
#[tokio::test]
async fn inflight_marker_lifecycle_and_expiry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("libra.db");
    let conn = libra::internal::db::create_database(&db_path.display().to_string())
        .await
        .expect("create db");

    let now_ms = 1_000_000i64;
    let live = TracesInflightMarker::new("sess-marker", "attempt-live", now_ms);
    history::write_traces_inflight_marker(&conn, &live)
        .await
        .expect("write live marker");

    let mut expired = TracesInflightMarker::new("sess-marker", "attempt-expired", now_ms);
    expired.started_at_ms = now_ms - history::AGENT_TRACES_INFLIGHT_TTL_MS - 1;
    history::write_traces_inflight_marker(&conn, &expired)
        .await
        .expect("write expired marker");

    let listed = history::list_live_traces_inflight_markers(&conn, now_ms)
        .await
        .expect("list live");
    assert_eq!(listed.len(), 1, "only the non-expired marker is live");
    assert_eq!(listed[0].attempt_id, "attempt-live");
    assert_eq!(listed[0].session_id, "sess-marker");

    // Re-writing the same (session, attempt) refreshes rather than
    // duplicates (UNIQUE(scope,target,key) upsert).
    let mut refreshed = live.clone();
    refreshed.commit = Some("abc123".to_string());
    history::write_traces_inflight_marker(&conn, &refreshed)
        .await
        .expect("refresh marker");
    let listed = history::list_live_traces_inflight_markers(&conn, now_ms)
        .await
        .expect("list live after refresh");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].commit.as_deref(), Some("abc123"));

    let cleared = history::clear_traces_inflight_marker(&conn, "sess-marker", "attempt-live")
        .await
        .expect("clear marker");
    assert!(cleared);
    assert!(
        history::list_live_traces_inflight_markers(&conn, now_ms)
            .await
            .expect("list live after clear")
            .is_empty()
    );
    // Clearing an absent marker reports false, not an error.
    assert!(
        !history::clear_traces_inflight_marker(&conn, "sess-marker", "attempt-live")
            .await
            .expect("double clear")
    );
}

// ---------------------------------------------------------------------------
// Legacy-v1 fixture readability (shared reader helpers must not regress)
// ---------------------------------------------------------------------------

/// The committed pre-AG-20 fixture (`tests/fixtures/agent_checkpoints/
/// v1_claude_code/`, captured at v0.18.6) stays readable through the
/// existing metadata-first reader path: seed its byte-identical blobs +
/// catalog rows into a fresh repo and drive `libra agent checkpoint show`
/// end-to-end. Full v1 fallback (manifest-less show/transcript) is the
/// reader slice's job; this guard pins that the shared
/// `load_metadata_blob` flow keeps accepting v1 checkpoints.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn v1_fixture_checkpoint_remains_readable_via_checkpoint_show() {
    use std::process::{Command, Stdio};

    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/agent_checkpoints/v1_claude_code/85/ae75d2-4c53-465a-b890-a9f861a50cc7",
    );
    let metadata_bytes =
        std::fs::read(fixture_root.join("metadata.json")).expect("fixture metadata");
    let transcript_bytes =
        std::fs::read(fixture_root.join("transcript/claude_code")).expect("fixture transcript");

    // Isolated repo via the real CLI (`libra init`).
    let tempdir = tempfile::tempdir().expect("tempdir");
    let home = tempdir.path().join("home");
    let repo = tempdir.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("LIBRA_TEST_HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn libra binary")
    };
    let init = run(&["init"]);
    assert!(init.status.success(), "libra init failed: {init:?}");

    // Byte-identical blob seeding — the OIDs must reproduce the values
    // pinned in the fixture README (provenance guard against hand-edits).
    let libra_dir = repo.join(".libra");
    let metadata_oid = libra::utils::object::write_git_object(&libra_dir, "blob", &metadata_bytes)
        .expect("write fixture metadata blob")
        .to_string();
    assert_eq!(
        metadata_oid, "b0265e8c5249c53dc588913554cdebdb82b984ec",
        "fixture metadata.json bytes must rehash to the README-pinned OID"
    );
    let transcript_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", &transcript_bytes)
            .expect("write fixture transcript blob")
            .to_string();
    assert_eq!(
        transcript_oid, "2c43a69258d78142464f074e4c050bd9c7f0325f",
        "fixture transcript blob bytes must rehash to the README-pinned OID"
    );

    // Seed the catalog rows the way the v1 writer would have.
    let db_url = format!("sqlite://{}", libra_dir.join("libra.db").display());
    let mut opts = sea_orm::ConnectOptions::new(db_url);
    opts.sqlx_logging(false);
    let conn = sea_orm::Database::connect(opts)
        .await
        .expect("open repo db");
    let backend = conn.get_database_backend();
    let session_id = "claude__fixture-v1-claude";
    let checkpoint_id = "85ae75d2-4c53-465a-b890-a9f861a50cc7";
    conn.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) VALUES (?, 'claude_code', ?, 'stopped', ?, 1, 1)",
        [
            session_id.into(),
            "fixture-v1-claude".into(),
            repo.display().to_string().into(),
        ],
    ))
    .await
    .expect("seed agent_session");
    conn.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, ?, 'committed', NULL, ?, ?, ?, 1)",
        [
            checkpoint_id.into(),
            session_id.into(),
            "188c5b1782588d9a1598dae491f5430ed16068c2".into(),
            metadata_oid.clone().into(),
            "64c851d2df4228ecd86e0d7aa54d1ba8c4fa4efc".into(),
        ],
    ))
    .await
    .expect("seed agent_checkpoint");
    drop(conn);

    // The metadata-first reader path must render the v1 checkpoint.
    let show = run(&["agent", "checkpoint", "show", checkpoint_id, "--json"]);
    assert!(show.status.success(), "checkpoint show failed: {show:?}");
    let stdout = String::from_utf8_lossy(&show.stdout).to_string();
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("show output is JSON");
    assert_eq!(
        parsed["data"]["checkpoint"]["checkpoint_id"],
        json!(checkpoint_id)
    );
    assert_eq!(
        parsed["data"]["metadata"]["schema_version"],
        json!(1),
        "v1 metadata must surface unchanged (schema_version 1)"
    );
    assert_eq!(
        parsed["data"]["metadata"]["agent_kind"],
        json!("claude_code")
    );
    // The fixture's redaction promise holds end-to-end: the raw token never
    // appears; the marker does (inside metadata.redaction_report matches).
    assert!(
        !stdout.contains(&format!("AKIA{}", "IOSFODNN7EXAMPLE")),
        "raw secret must not surface through checkpoint show"
    );
}

/// End-to-end: a successful checkpoint ingest leaves NO in-flight marker
/// behind (written before stage (a), cleared after stage (d)).
#[tokio::test]
#[serial]
async fn successful_ingest_clears_its_inflight_marker() {
    let repo = ExportRepo::init(SMALL_TRANSCRIPT).await;
    let row = repo.ingest_session("sess-marker-e2e", json!({})).await;
    assert!(!row.traces_commit.is_empty());

    let backend = repo.conn.get_database_backend();
    let leftover = repo
        .conn
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_traces_inflight'"
                .to_string(),
        ))
        .await
        .expect("count markers")
        .expect("count row");
    assert_eq!(
        leftover.try_get_by::<i64, _>("n").unwrap(),
        0,
        "a successful write must clear its window guard"
    );
}
