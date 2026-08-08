//! AG-20 reader-slice tests (plan.md Task A5): keyset pagination for
//! `agent checkpoint list` / `agent session list`, the metadata-first
//! `checkpoint show` layout summary (E4-libra manifest-first plus the
//! legacy-v1 manifest-less fallback), and the index-hit validation for the
//! `2026070802_agent_checkpoint_paging` migration.
//!
//! Follows the E2E harness conventions of `tests/agent_lifecycle_event_test.rs`:
//! `libra init` in a tempdir via the built binary, hook envelopes piped on
//! stdin for real ingests, and assertions on the observable CLI JSON
//! surfaces. Catalog rows for the pure pagination tests are seeded through
//! a direct SQLite connection so the walk can cover 100+ checkpoints
//! without 100+ ingests.
//!
//! Covered contracts:
//!
//! - default page size 50, hard cap 500 (larger `--limit` clamps with a
//!   stderr note), `--limit 0` treated as 1;
//! - opaque keyset cursor (base64 `v1:<ts>:<id>`) walks pages with no
//!   overlap and no gap; `next_cursor` is `null` exactly when exhausted;
//!   malformed cursors fail closed with an actionable `--cursor` error;
//! - `checkpoint show` classifies BOTH layouts: E4-libra (manifest-first
//!   role/part summary, `content_hash` format check via the bare-hex
//!   tolerant parser) and the committed pre-AG-20 v1 fixture
//!   (`legacy-v1`, metadata-only fallback);
//! - metadata-first discipline: `show` never reads transcript bodies, so
//!   deleting a transcript blob from the object store flips availability
//!   to `missing` instead of erroring;
//! - EXPLAIN QUERY PLAN on the paginated queries against a real
//!   `libra init` repository database hits the
//!   `idx_agent_checkpoint_created_paging` /
//!   `idx_agent_session_started_paging` indexes with no table scan and no
//!   temp B-tree.

#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One isolated libra repository plus a fake `$HOME` for provider
/// transcript roots (`~/.claude`), driven end-to-end through the built
/// `libra` binary.
struct ReaderRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl ReaderRepo {
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
        let out = this.run(&["init"], None, &[]);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    /// Run the built `libra` binary inside the repo with a clean
    /// environment plus `extra_envs` (e.g. the E5 chunk-threshold test
    /// override for the writer).
    fn run(&self, args: &[&str], stdin: Option<&str>, extra_envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
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
        for (key, value) in extra_envs {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn libra binary");
        if let Some(payload) = stdin {
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write stdin payload");
        }
        child.wait_with_output().expect("wait for libra binary")
    }

    /// `libra agent hooks <agent> <verb>` with `envelope` piped via stdin.
    fn hook(&self, agent: &str, verb: &str, envelope: &str, extra_envs: &[(&str, &str)]) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope), extra_envs)
    }

    fn libra_dir(&self) -> PathBuf {
        self.repo.join(".libra")
    }

    /// Direct connection to the repository's SQLite catalog (the same
    /// `.libra/libra.db` the CLI reads), for row seeding and EXPLAIN.
    async fn db(&self) -> DatabaseConnection {
        let url = format!("sqlite://{}", self.libra_dir().join("libra.db").display());
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        Database::connect(opts).await.expect("open repo db")
    }

    /// Canonical hook envelope with the repo as `cwd`.
    fn envelope(&self, hook_event_name: &str, session_id: &str, transcript_path: &Path) -> String {
        json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
            "transcript_path": transcript_path.to_string_lossy(),
        })
        .to_string()
    }

    /// Write a transcript under the fake home's `~/.claude` (the Claude
    /// Code provider's protected dir) so the checkpoint writer's
    /// provider-root trust gate accepts it.
    fn write_claude_transcript(&self, content: &[u8]) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("x");
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, content).expect("write transcript fixture");
        path
    }

    /// Delete one loose object file from the repo's object store —
    /// simulates a transcript blob that is unavailable locally.
    fn delete_loose_object(&self, oid: &str) {
        let path = self
            .libra_dir()
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("delete object {oid} at {}: {e}", path.display()));
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

/// Parse a `--json` envelope, asserting success and `ok: true`.
fn json_data(out: &Output) -> Value {
    assert!(out.status.success(), "CLI query failed: {}", describe(out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout is not JSON ({err}): {stdout}"));
    assert_eq!(parsed["ok"], json!(true), "envelope not ok: {parsed}");
    parsed["data"].clone()
}

/// One page of `checkpoint list --json` / `session list --json`: rows
/// under `rows_key` plus the `next_cursor` (None when null/exhausted).
fn list_page(out: &Output, rows_key: &str) -> (Vec<Value>, Option<String>) {
    let data = json_data(out);
    assert_eq!(
        data["schema_version"],
        json!(1),
        "paged list payload must carry schema_version 1: {data}"
    );
    let rows = data[rows_key]
        .as_array()
        .unwrap_or_else(|| panic!("data.{rows_key} is not an array: {data}"))
        .clone();
    let next_cursor = data["next_cursor"].as_str().map(str::to_string);
    (rows, next_cursor)
}

// ---------------------------------------------------------------------------
// Catalog seeding (direct DB inserts, mirroring the hook writer's columns)
// ---------------------------------------------------------------------------

async fn seed_session(conn: &DatabaseConnection, session_id: &str, started_at: i64) {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) \
         VALUES (?, 'claude_code', ?, 'stopped', '/tmp/repo', ?, ?)",
        [
            session_id.into(),
            format!("{session_id}-provider").into(),
            started_at.into(),
            started_at.into(),
        ],
    ))
    .await
    .expect("seed agent_session");
}

/// Insert checkpoints in batches (multi-row VALUES stay under SQLite's
/// bind-parameter cap).
async fn seed_checkpoints(conn: &DatabaseConnection, session_id: &str, rows: &[(String, i64)]) {
    let backend = conn.get_database_backend();
    for batch in rows.chunks(100) {
        let mut sql = String::from(
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
             tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES ",
        );
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(batch.len() * 5);
        for (index, (checkpoint_id, created_at)) in batch.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push_str("(?, ?, 'committed', NULL, 'seed-tree', 'seed-meta', ?, ?)");
            values.push(checkpoint_id.clone().into());
            values.push(session_id.into());
            values.push(format!("commit-{checkpoint_id}").into());
            values.push((*created_at).into());
        }
        conn.execute_raw(Statement::from_sql_and_values(backend, &sql, values))
            .await
            .expect("seed agent_checkpoint batch");
    }
}

/// Newest-first keyset order: `(timestamp DESC, id ASC)` — the exact shape
/// of the 2026070802 pagination indexes.
fn sort_keyset(rows: &mut [(String, i64)]) {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
}

// ---------------------------------------------------------------------------
// Pagination: cursor walk, clamp, malformed cursors
// ---------------------------------------------------------------------------

/// Seed 120 checkpoints (with shared timestamps so the id tiebreaker is
/// exercised) and walk `checkpoint list` in default-limit pages: 50 + 50 +
/// 20, no overlap, no gap, `next_cursor` null exactly at the end.
#[tokio::test]
async fn checkpoint_list_walks_keyset_pages_without_overlap_or_gap() {
    let repo = ReaderRepo::init();
    let conn = repo.db().await;
    seed_session(&conn, "sess-page", 1).await;
    // Groups of 4 share a created_at so pages must fall back to the
    // checkpoint_id tiebreaker inside a timestamp.
    let mut seeded: Vec<(String, i64)> = (0..120)
        .map(|index| (format!("cp-{index:03}"), 1_000 + (index / 4) as i64))
        .collect();
    seed_checkpoints(&conn, "sess-page", &seeded).await;
    drop(conn);

    sort_keyset(&mut seeded);
    let expected_ids: Vec<&str> = seeded.iter().map(|(id, _)| id.as_str()).collect();

    let mut walked_ids: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_sizes: Vec<usize> = Vec::new();
    loop {
        let mut args = vec!["agent", "checkpoint", "list", "--json"];
        if let Some(cursor) = &cursor {
            args.extend_from_slice(&["--cursor", cursor]);
        }
        let out = repo.run(&args, None, &[]);
        let (rows, next) = list_page(&out, "checkpoints");
        page_sizes.push(rows.len());
        for row in &rows {
            // Row schema stays the full pre-pagination shape (additive
            // change only).
            for key in [
                "checkpoint_id",
                "session_id",
                "scope",
                "parent_commit",
                "tree_oid",
                "metadata_blob_oid",
                "traces_commit",
                "created_at",
            ] {
                assert!(row.get(key).is_some(), "row key '{key}' missing from {row}");
            }
            walked_ids.push(row["checkpoint_id"].as_str().expect("id str").to_string());
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(walked_ids.len() <= 120, "cursor loop must terminate");
    }

    assert_eq!(page_sizes, vec![50, 50, 20], "default limit is 50");
    assert_eq!(
        walked_ids, expected_ids,
        "walk must produce every row exactly once in (created_at DESC, checkpoint_id ASC) order"
    );
}

/// `--limit` above the 500 cap clamps with a stderr note (stdout stays a
/// clean JSON page); `--limit 0` is treated as 1.
#[tokio::test]
async fn checkpoint_list_clamps_limit_and_floors_zero() {
    let repo = ReaderRepo::init();
    let conn = repo.db().await;
    seed_session(&conn, "sess-clamp", 1).await;
    let seeded: Vec<(String, i64)> = (0..505)
        .map(|index| (format!("cp-{index:03}"), 2_000 + index as i64))
        .collect();
    seed_checkpoints(&conn, "sess-clamp", &seeded).await;
    drop(conn);

    let out = repo.run(
        &["agent", "checkpoint", "list", "--json", "--limit", "501"],
        None,
        &[],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("--limit 501") && stderr.contains("500"),
        "clamp note must land on stderr: {stderr}"
    );
    let (rows, next) = list_page(&out, "checkpoints");
    assert_eq!(rows.len(), 500, "clamped to the hard cap");
    let next = next.expect("505 rows > 500 → another page exists");

    let out = repo.run(
        &[
            "agent",
            "checkpoint",
            "list",
            "--json",
            "--limit",
            "501",
            "--cursor",
            &next,
        ],
        None,
        &[],
    );
    let (rows, next) = list_page(&out, "checkpoints");
    assert_eq!(rows.len(), 5, "second page carries the remainder");
    assert!(next.is_none(), "listing exhausted → next_cursor null");

    // --limit 0 → smallest page is still a page.
    let out = repo.run(
        &["agent", "checkpoint", "list", "--json", "--limit", "0"],
        None,
        &[],
    );
    let (rows, next) = list_page(&out, "checkpoints");
    assert_eq!(rows.len(), 1, "--limit 0 is treated as 1");
    assert!(next.is_some());
}

/// Malformed cursors fail closed with an actionable usage error naming
/// `--cursor` — for both list surfaces.
#[test]
fn list_rejects_malformed_cursors() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let repo = ReaderRepo::init();
    let wrong_version = STANDARD.encode("v2:1:x");
    let bad_timestamp = STANDARD.encode("v1:notanumber:x");
    let cases = [
        ("checkpoint", "definitely-not-base64!!"),
        ("checkpoint", wrong_version.as_str()),
        ("session", bad_timestamp.as_str()),
    ];
    for (surface, cursor) in cases {
        let out = repo.run(
            &["agent", surface, "list", "--json", "--cursor", cursor],
            None,
            &[],
        );
        assert!(
            !out.status.success(),
            "malformed cursor '{cursor}' must fail {surface} list: {}",
            describe(&out)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--cursor"),
            "error must name --cursor: {}",
            describe(&out)
        );
    }
}

/// `session list` pages over `(started_at DESC, session_id ASC)` with the
/// same cursor contract, and filters still compose with the cursor.
#[tokio::test]
async fn session_list_walks_keyset_pages() {
    let repo = ReaderRepo::init();
    let conn = repo.db().await;
    // Pairs share a started_at so the session_id tiebreaker is exercised.
    let mut seeded: Vec<(String, i64)> = (0..7)
        .map(|index| (format!("sess-{index}"), 100 + (index / 2) as i64))
        .collect();
    for (session_id, started_at) in &seeded {
        seed_session(&conn, session_id, *started_at).await;
    }
    drop(conn);
    sort_keyset(&mut seeded);
    let expected_ids: Vec<&str> = seeded.iter().map(|(id, _)| id.as_str()).collect();

    let mut walked: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_sizes = Vec::new();
    loop {
        let mut args = vec![
            "agent", "session", "list", "--json", "--limit", "3", "--state", "stopped",
        ];
        if let Some(cursor) = &cursor {
            args.extend_from_slice(&["--cursor", cursor]);
        }
        let out = repo.run(&args, None, &[]);
        let (rows, next) = list_page(&out, "sessions");
        page_sizes.push(rows.len());
        for row in &rows {
            walked.push(row["session_id"].as_str().expect("id str").to_string());
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(walked.len() <= 7, "cursor loop must terminate");
    }
    assert_eq!(page_sizes, vec![3, 3, 1]);
    assert_eq!(walked, expected_ids);
}

// ---------------------------------------------------------------------------
// `checkpoint show`: legacy-v1 fixture classification
// ---------------------------------------------------------------------------

/// Reconstruct the committed pre-AG-20 fixture
/// (`tests/fixtures/agent_checkpoints/v1_claude_code/`) inside a fresh
/// repo — byte-identical blobs (OIDs re-verified against the fixture
/// README) plus the v1 tree chain — and assert `checkpoint show`
/// classifies it as `legacy-v1` with a metadata-only fallback summary,
/// never an E4-libra inconsistency. Deleting the transcript blob must
/// flip availability to `missing` without failing the show.
#[tokio::test]
async fn v1_fixture_show_classifies_legacy_layout() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/agent_checkpoints/v1_claude_code/85/ae75d2-4c53-465a-b890-a9f861a50cc7",
    );
    let metadata_bytes =
        std::fs::read(fixture_root.join("metadata.json")).expect("fixture metadata");
    let transcript_bytes =
        std::fs::read(fixture_root.join("transcript/claude_code")).expect("fixture transcript");

    let repo = ReaderRepo::init();
    let libra_dir = repo.libra_dir();

    // Blobs must rehash to the README-pinned OIDs (provenance guard).
    let metadata_oid = libra::utils::object::write_git_object(&libra_dir, "blob", &metadata_bytes)
        .expect("write metadata blob")
        .to_string();
    assert_eq!(metadata_oid, "b0265e8c5249c53dc588913554cdebdb82b984ec");
    let transcript_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", &transcript_bytes)
            .expect("write transcript blob")
            .to_string();
    assert_eq!(transcript_oid, "2c43a69258d78142464f074e4c050bd9c7f0325f");

    // v1 tree chain: transcript/ → inner → <id[2..]> → <id[..2]> → root,
    // exactly the splice shape the pre-AG-20 writer produced.
    let checkpoint_id = "85ae75d2-4c53-465a-b890-a9f861a50cc7";
    let transcript_tree = write_tree(&libra_dir, &[("100644", "claude_code", &transcript_oid)]);
    let inner_tree = write_tree(
        &libra_dir,
        &[
            ("100644", "metadata.json", &metadata_oid),
            ("40000", "transcript", &transcript_tree),
        ],
    );
    let prefix_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[2..], &inner_tree)]);
    let checkpoint_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[..2], &prefix_tree)]);
    let root_tree = write_tree(&libra_dir, &[("40000", "checkpoint", &checkpoint_tree)]);
    // The reconstruction must reproduce the capture repo's root tree OID
    // pinned in the fixture README — proving the fixture bytes and the v1
    // tree shape assumed here are the real pre-AG-20 writer output.
    assert_eq!(
        root_tree, "188c5b1782588d9a1598dae491f5430ed16068c2",
        "v1 tree reconstruction must rehash to the README-pinned tree_oid"
    );

    let conn = repo.db().await;
    seed_session(&conn, "claude__fixture-v1-claude", 1).await;
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, 'claude__fixture-v1-claude', 'committed', NULL, ?, ?, \
                 '64c851d2df4228ecd86e0d7aa54d1ba8c4fa4efc', 1783206712)",
        [
            checkpoint_id.into(),
            root_tree.clone().into(),
            metadata_oid.clone().into(),
        ],
    ))
    .await
    .expect("seed agent_checkpoint");
    drop(conn);

    let show = repo.run(
        &["agent", "checkpoint", "show", checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    assert_eq!(data["checkpoint"]["checkpoint_id"], json!(checkpoint_id));
    assert_eq!(
        data["metadata"]["schema_version"],
        json!(1),
        "v1 metadata must surface unchanged"
    );
    let layout = &data["layout"];
    assert_eq!(
        layout["kind"],
        json!("legacy-v1"),
        "manifest-less v1 tree must classify as legacy-v1, got: {layout}"
    );
    assert_eq!(layout["transcript"]["availability"], json!("present"));
    assert_eq!(layout["transcript"]["chunked"], json!(false));
    assert_eq!(
        layout["transcript"]["parts"][0]["path"],
        json!("transcript/claude_code"),
        "v1 transcript keeps its extension-less provider path"
    );
    assert!(
        layout["transcript"]["parts"][0]["byte_len"].is_null(),
        "v1 part sizes are unknown by design (sizing would read the blob)"
    );
    assert!(
        layout["content_hash"].is_null(),
        "v1 has no content_hash.txt — must be null, not an error"
    );
    let roles: Vec<&str> = layout["roles"]
        .as_array()
        .expect("roles array")
        .iter()
        .filter_map(|role| role["role"].as_str())
        .collect();
    assert!(roles.contains(&"metadata"), "roles: {roles:?}");
    assert!(roles.contains(&"transcript"), "roles: {roles:?}");

    // Human output names the layout too.
    let human = repo.run(&["agent", "checkpoint", "show", checkpoint_id], None, &[]);
    assert!(human.status.success(), "{}", describe(&human));
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        stdout.contains("legacy-v1"),
        "human show must name the layout: {stdout}"
    );

    // Metadata-first discipline: a missing transcript blob degrades
    // availability, never the command.
    repo.delete_loose_object(&transcript_oid);
    let show = repo.run(
        &["agent", "checkpoint", "show", checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    assert_eq!(data["layout"]["kind"], json!("legacy-v1"));
    assert_eq!(
        data["layout"]["transcript"]["availability"],
        json!("missing"),
        "absent transcript blob → availability missing, not an error"
    );
    assert_eq!(
        data["metadata"]["schema_version"],
        json!(1),
        "metadata summary must survive the missing transcript"
    );
}

/// Serialize one git tree object from pre-sorted `(mode, name, oid_hex)`
/// entries and write it to the loose-object store, returning its OID.
fn write_tree(libra_dir: &Path, entries: &[(&str, &str, &str)]) -> String {
    let mut body = Vec::new();
    for (mode, name, oid) in entries {
        body.extend_from_slice(mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&hex::decode(oid).expect("valid oid hex"));
    }
    libra::utils::object::write_git_object(libra_dir, "tree", &body)
        .expect("write tree object")
        .to_string()
}

// ---------------------------------------------------------------------------
// `checkpoint show`: E4-libra manifest summary (chunked + missing blob)
// ---------------------------------------------------------------------------

/// Run one real hook ingest (SessionStart + Stop) and return the resulting
/// checkpoint id from `checkpoint list --json`.
fn ingest_one_checkpoint(repo: &ReaderRepo, transcript: &[u8], envs: &[(&str, &str)]) -> String {
    let transcript_path = repo.write_claude_transcript(transcript);
    let session = "sess-reader-e4";
    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", session, &transcript_path),
        envs,
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));
    let out = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope("Stop", session, &transcript_path),
        envs,
    );
    assert!(out.status.success(), "stop: {}", describe(&out));

    let list = repo.run(&["agent", "checkpoint", "list", "--json"], None, &[]);
    let (rows, _) = list_page(&list, "checkpoints");
    assert_eq!(rows.len(), 1, "one ingest → one checkpoint: {rows:?}");
    rows[0]["checkpoint_id"]
        .as_str()
        .expect("checkpoint_id")
        .to_string()
}

/// A chunked E4-libra checkpoint (written under the E5 test threshold
/// override) shows its parts in manifest order without loading them, and
/// a deleted chunk flips availability to `missing` while the manifest
/// summary keeps rendering.
#[test]
fn chunked_show_lists_parts_in_manifest_order_without_loading() {
    // ~40 bytes per line × 40 lines ≈ 1.5 KiB; threshold 256 → ≥ 6 parts.
    let mut transcript = Vec::new();
    for index in 0..40 {
        transcript.extend_from_slice(
            format!("{{\"turn\":{index:04},\"text\":\"chunk me\"}}\n").as_bytes(),
        );
    }
    let repo = ReaderRepo::init();
    let envs = [("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD", "256")];
    let checkpoint_id = ingest_one_checkpoint(&repo, &transcript, &envs);

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    let layout = &data["layout"];
    assert_eq!(layout["kind"], json!("e4-libra"));
    assert_eq!(layout["transcript"]["chunked"], json!(true));
    assert_eq!(layout["transcript"]["availability"], json!("present"));
    let parts = layout["transcript"]["parts"].as_array().expect("parts");
    assert!(parts.len() > 1, "chunked transcript must list >1 part");
    let mut total_len = 0u64;
    for (index, part) in parts.iter().enumerate() {
        assert_eq!(
            part["path"],
            json!(format!("transcript/claude_code.jsonl.{:03}", index + 1)),
            "parts must surface in manifest (.%03d) order"
        );
        assert!(part["oid"].as_str().is_some(), "part carries its oid");
        total_len += part["byte_len"].as_u64().expect("declared byte_len");
    }
    assert_eq!(
        total_len,
        transcript.len() as u64,
        "declared part lengths must add up to the logical transcript"
    );
    assert_eq!(
        layout["content_hash"]["format_valid"],
        json!(true),
        "content_hash.txt must parse via the sha256:/bare-hex reader"
    );
    let digest = layout["content_hash"]["digest"].as_str().expect("digest");
    assert_eq!(digest.len(), 64);

    // Metadata-first discipline for the chunked path: delete ONE chunk —
    // show still succeeds from the manifest, availability flips.
    let deleted_oid = parts[1]["oid"].as_str().expect("part oid").to_string();
    repo.delete_loose_object(&deleted_oid);
    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    assert_eq!(data["layout"]["kind"], json!("e4-libra"));
    assert_eq!(
        data["layout"]["transcript"]["availability"],
        json!("missing")
    );
    assert_eq!(
        data["layout"]["transcript"]["parts"]
            .as_array()
            .expect("parts")
            .len(),
        parts.len(),
        "part listing comes from the manifest, not from surviving blobs"
    );
}

/// Single-file E4-libra checkpoint: `show` summarizes the manifest roles,
/// and an intentionally deleted transcript blob is reported as `missing`
/// (with metadata/manifest still rendered) rather than erroring — the
/// direct assertion that `show` never reads the transcript body.
#[test]
fn show_survives_missing_transcript_blob() {
    let transcript =
        b"{\"role\":\"user\",\"text\":\"kick off\"}\n{\"role\":\"assistant\",\"text\":\"done\"}\n";
    let repo = ReaderRepo::init();
    let checkpoint_id = ingest_one_checkpoint(&repo, transcript, &[]);

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    let layout = &data["layout"];
    assert_eq!(layout["kind"], json!("e4-libra"));
    assert_eq!(layout["transcript"]["chunked"], json!(false));
    assert_eq!(layout["transcript"]["availability"], json!("present"));
    let roles: Vec<&str> = layout["roles"]
        .as_array()
        .expect("roles")
        .iter()
        .filter_map(|role| role["role"].as_str())
        .collect();
    for role in [
        "content_hash",
        "lifecycle_events",
        "metadata",
        "redaction_report",
        "transcript",
    ] {
        assert!(roles.contains(&role), "role {role} missing from {roles:?}");
    }
    let transcript_oid = layout["transcript"]["parts"][0]["oid"]
        .as_str()
        .expect("transcript oid")
        .to_string();

    repo.delete_loose_object(&transcript_oid);
    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
        &[],
    );
    assert!(
        show.status.success(),
        "show must not fail on a missing transcript blob: {}",
        describe(&show)
    );
    let data = json_data(&show);
    assert_eq!(
        data["layout"]["transcript"]["availability"],
        json!("missing")
    );
    assert_eq!(
        data["layout"]["content_hash"]["format_valid"],
        json!(true),
        "manifest-side summary survives the missing transcript"
    );
    assert!(
        data["metadata"].is_object(),
        "metadata.json summary survives the missing transcript"
    );
}

// ---------------------------------------------------------------------------
// Index-hit validation (plan.md A5 validation row)
// ---------------------------------------------------------------------------

/// EXPLAIN QUERY PLAN for the cursored page queries against a REAL
/// `libra init` repository database (i.e. through the production
/// migration path, not a synthetic schema): both must be index SEARCHes
/// on the 2026070802 pagination indexes — no table SCAN, no temp B-tree.
///
/// The SQL literals mirror `checkpoint_page_sql` / `session_page_sql` in
/// `src/command/agent/{checkpoint,session}.rs`; the in-crate unit test
/// `command::agent::checkpoint::tests::paginated_list_queries_hit_keyset_indexes`
/// runs the same assertion on the builders themselves, guarding drift.
#[tokio::test]
async fn explain_query_plan_hits_pagination_indexes_on_repo_db() {
    let repo = ReaderRepo::init();
    let conn = repo.db().await;
    seed_session(&conn, "sess-eqp", 10).await;
    seed_checkpoints(
        &conn,
        "sess-eqp",
        &[("cp-a".to_string(), 10), ("cp-b".to_string(), 11)],
    )
    .await;

    let backend = conn.get_database_backend();
    let cases: [(&str, &str, &str); 2] = [
        (
            "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
             metadata_blob_oid, traces_commit, created_at \
             FROM agent_checkpoint WHERE 1=1 \
             AND (created_at < ? OR (created_at = ? AND checkpoint_id > ?)) \
             ORDER BY created_at DESC, checkpoint_id ASC LIMIT ?",
            "idx_agent_checkpoint_created_paging",
            "agent_checkpoint",
        ),
        (
            "SELECT session_id, agent_kind, state, working_dir, started_at, last_event_at \
             FROM agent_session WHERE 1=1 \
             AND (started_at < ? OR (started_at = ? AND session_id > ?)) \
             ORDER BY started_at DESC, session_id ASC LIMIT ?",
            "idx_agent_session_started_paging",
            "agent_session",
        ),
    ];
    for (sql, index_name, table) in cases {
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                format!("EXPLAIN QUERY PLAN {sql}"),
                [
                    11i64.into(),
                    11i64.into(),
                    "cp-a".to_string().into(),
                    51i64.into(),
                ],
            ))
            .await
            .expect("explain query plan");
        let plan = rows
            .iter()
            .map(|row| row.try_get_by::<String, _>("detail").unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plan.contains(index_name),
            "plan for {table} pagination must use {index_name}, got:\n{plan}"
        );
        assert!(
            !plan.contains("TEMP B-TREE"),
            "plan for {table} pagination must not sort via temp B-tree, got:\n{plan}"
        );
        for line in plan.lines() {
            // A `SCAN <table>` step is only acceptable when it goes
            // through an index (`... USING INDEX ...`).
            assert!(
                !line.trim_start().starts_with(&format!("SCAN {table}")) || line.contains("USING"),
                "plan for {table} pagination must not full-scan, got:\n{plan}"
            );
        }
    }
}
