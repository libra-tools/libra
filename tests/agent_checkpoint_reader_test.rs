//! AG-20 reader-slice tests (plan.md Task A5): keyset pagination for
//! `agent checkpoint list` / `agent session list`, the stable safe-summary
//! `checkpoint show` response, and the index-hit validation for the
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
//! - `checkpoint show` returns only its fixed safe structural summary for
//!   both current and pre-AG-20 checkpoint trees; it never reads or renders
//!   arbitrary metadata JSON or catalog object identifiers;
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

use libra::internal::ai::observed_agents::claude_project_slug;
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
        // Hook ingress canonicalizes cwd before deriving the provider-native
        // Claude source. Retain the same spelling in the fixture on macOS,
        // where `/var` is commonly an alias for `/private/var`.
        let this = Self {
            _tempdir: tempdir,
            repo: repo.canonicalize().expect("canonical repo dir"),
            home: home.canonicalize().expect("canonical fake home"),
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
    /// environment plus explicitly requested fixture variables.
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
    fn write_claude_transcript(&self, session_id: &str, content: &[u8]) -> PathBuf {
        let dir = self
            .home
            .join(".claude")
            .join("projects")
            .join(claude_project_slug(&self.repo));
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join(format!("{session_id}.jsonl"));
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
// `checkpoint show`: legacy-v1 safe-summary compatibility
// ---------------------------------------------------------------------------

/// Reconstruct the committed pre-AG-20 fixture
/// (`tests/fixtures/agent_checkpoints/v1_claude_code/`) inside a fresh
/// repo — byte-identical blobs (OIDs re-verified against the fixture
/// README) plus the v1 tree chain — and assert `checkpoint show` remains
/// readable without exposing the legacy metadata body or an object-layout
/// JSON contract.
#[tokio::test]
async fn v1_fixture_show_preserves_safe_summary_without_layout_contract() {
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
    assert!(
        data.get("layout").is_none(),
        "checkpoint show must not add an unversioned object-layout schema: {data}"
    );
    assert!(
        data.get("metadata").is_none(),
        "checkpoint show must not surface arbitrary v1 metadata: {data}"
    );

    // The safe summary remains readable even if an unrelated legacy
    // transcript blob has disappeared: default show never reads it.
    repo.delete_loose_object(&transcript_oid);
    let show = repo.run(
        &["agent", "checkpoint", "show", checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    assert_eq!(
        data["checkpoint"]["checkpoint_id"],
        json!(checkpoint_id),
        "safe summary must survive the missing transcript"
    );
}

/// `checkpoint show` is a deliberately narrow public read surface.  A local
/// catalog or legacy metadata blob may contain source locators, commitments,
/// redaction details, or internal object identities; neither human nor JSON
/// output may serialize any of them by default.
#[tokio::test]
async fn checkpoint_show_hides_catalog_internals_and_metadata_in_human_and_json() {
    const CHECKPOINT_ID: &str = "show-safe-summary-0000-0000-0000-000000000001";
    const SESSION_ID: &str = "session-private-sentinel";
    const LOCATOR_SENTINEL: &str = "provider-locator-sentinel";
    const COMMITMENT_SENTINEL: &str = "source-commitment-sentinel";
    const DIGEST_SENTINEL: &str = "content-digest-sentinel";
    const REDACTION_SENTINEL: &str = "redaction-detail-sentinel";

    let repo = ReaderRepo::init();
    let metadata = json!({
        "source_locator": LOCATOR_SENTINEL,
        "source_commitment": COMMITMENT_SENTINEL,
        "content_hash": DIGEST_SENTINEL,
        "redaction_report": { "detail": REDACTION_SENTINEL },
    })
    .to_string();
    let metadata_oid =
        libra::utils::object::write_git_object(&repo.libra_dir(), "blob", metadata.as_bytes())
            .expect("write sensitive metadata fixture")
            .to_string();
    let tree_oid = "a".repeat(40);
    let traces_commit = "c".repeat(40);
    let parent_commit = "d".repeat(40);
    let conn = repo.db().await;
    seed_session(&conn, SESSION_ID, 42).await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, ?, 'committed', ?, ?, ?, ?, 42)",
        [
            CHECKPOINT_ID.into(),
            SESSION_ID.into(),
            parent_commit.clone().into(),
            tree_oid.clone().into(),
            metadata_oid.clone().into(),
            traces_commit.clone().into(),
        ],
    ))
    .await
    .expect("seed private checkpoint catalog row");
    drop(conn);

    let json_output = repo.run(
        &["agent", "checkpoint", "show", CHECKPOINT_ID, "--json"],
        None,
        &[],
    );
    let data = json_data(&json_output);
    let data_object = data.as_object().expect("checkpoint show data object");
    let checkpoint = data["checkpoint"]
        .as_object()
        .expect("checkpoint safe summary object");
    let mut data_keys = data_object.keys().map(String::as_str).collect::<Vec<_>>();
    data_keys.sort_unstable();
    assert_eq!(
        data_keys,
        ["checkpoint"],
        "show JSON must retain only its documented top-level summary: {data}"
    );
    let mut keys = checkpoint.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "checkpoint_id",
            "created_at",
            "parent_snapshot_recorded",
            "scope",
        ],
        "show JSON must retain only its documented whitelist: {data}"
    );
    assert_eq!(checkpoint["checkpoint_id"], json!(CHECKPOINT_ID));
    assert_eq!(checkpoint["scope"], json!("committed"));
    assert_eq!(checkpoint["created_at"], json!(42));
    assert_eq!(checkpoint["parent_snapshot_recorded"], json!(true));
    assert!(
        data.get("metadata").is_none(),
        "metadata must be withheld: {data}"
    );

    let human_output = repo.run(&["agent", "checkpoint", "show", CHECKPOINT_ID], None, &[]);
    assert!(
        human_output.status.success(),
        "human checkpoint show failed: {}",
        describe(&human_output)
    );
    let json_text = String::from_utf8_lossy(&json_output.stdout);
    let human_text = String::from_utf8_lossy(&human_output.stdout);
    assert_eq!(
        human_text,
        format!(
            "checkpoint_id             : {CHECKPOINT_ID}\n\
             scope                     : committed\n\
             created_at                : 42\n\
             parent_snapshot_recorded  : yes\n"
        ),
        "human show must retain only its documented fixed summary"
    );
    for private_value in [
        SESSION_ID,
        LOCATOR_SENTINEL,
        COMMITMENT_SENTINEL,
        DIGEST_SENTINEL,
        REDACTION_SENTINEL,
        tree_oid.as_str(),
        metadata_oid.as_str(),
        traces_commit.as_str(),
        parent_commit.as_str(),
    ] {
        assert!(
            !json_text.contains(private_value) && !human_text.contains(private_value),
            "checkpoint show leaked private catalog or metadata value {private_value:?}; \
             json={json_text}; human={human_text}"
        );
    }
}

/// A safe summary must fail closed when its mandatory catalog fields are
/// corrupt. Substituting `0` or `false` would make damaged data look like a
/// plausible 1970 checkpoint without a parent snapshot.
#[tokio::test]
async fn checkpoint_show_rejects_corrupt_summary_columns_without_echoing_them() {
    const CHECKPOINT_ID: &str = "show-corrupt-summary-0000-0000-000000000001";
    const SESSION_ID: &str = "show-corrupt-summary-session";
    const CREATED_AT_SENTINEL: &str = "not-a-unix-timestamp";

    let repo = ReaderRepo::init();
    let conn = repo.db().await;
    seed_session(&conn, SESSION_ID, 42).await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, ?, 'committed', NULL, ?, ?, ?, 42)",
        [
            CHECKPOINT_ID.into(),
            SESSION_ID.into(),
            "a".repeat(40).into(),
            "b".repeat(40).into(),
            "c".repeat(40).into(),
        ],
    ))
    .await
    .expect("seed checkpoint summary row");

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "UPDATE agent_checkpoint SET parent_commit = X'0102' WHERE checkpoint_id = '{CHECKPOINT_ID}'"
        ),
    ))
    .await
    .expect("corrupt nullable parent summary column");
    drop(conn);

    let parent_output = repo.run(
        &["agent", "checkpoint", "show", CHECKPOINT_ID, "--json"],
        None,
        &[],
    );
    assert!(
        !parent_output.status.success(),
        "corrupt parent column must fail closed: {}",
        describe(&parent_output)
    );
    let parent_stderr = String::from_utf8_lossy(&parent_output.stderr);
    assert!(parent_stderr.contains("LBR-AGENT-009"));
    assert!(
        !parent_stderr.contains("0102"),
        "safe corruption diagnostic must not echo the raw parent value: {parent_stderr}"
    );

    let conn = repo.db().await;
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "UPDATE agent_checkpoint SET parent_commit = NULL, created_at = '{CREATED_AT_SENTINEL}' \
             WHERE checkpoint_id = '{CHECKPOINT_ID}'"
        ),
    ))
    .await
    .expect("corrupt mandatory timestamp summary column");
    drop(conn);

    let timestamp_output = repo.run(
        &["agent", "checkpoint", "show", CHECKPOINT_ID, "--json"],
        None,
        &[],
    );
    assert!(
        !timestamp_output.status.success(),
        "corrupt timestamp column must fail closed: {}",
        describe(&timestamp_output)
    );
    let timestamp_stderr = String::from_utf8_lossy(&timestamp_output.stderr);
    assert!(timestamp_stderr.contains("LBR-AGENT-009"));
    assert!(
        !timestamp_stderr.contains(CREATED_AT_SENTINEL),
        "safe corruption diagnostic must not echo the raw timestamp: {timestamp_stderr}"
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
    let session = "sess-reader-e4";
    let transcript_path = repo.write_claude_transcript(session, transcript);
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

/// Seed a small, production-shaped E4 chunked tree directly. The writer's
/// small-threshold chunking behavior is exercised in the in-crate history
/// test; this fixture keeps the CLI reader contract independent of any
/// process environment override.
async fn seed_chunked_e4_checkpoint(repo: &ReaderRepo, transcript: &[u8]) -> String {
    let checkpoint_id = "e5reader-0000-0000-0000-000000000001";
    let libra_dir = repo.libra_dir();
    let metadata = json!({
        "schema_version": 2,
        "checkpoint_id": checkpoint_id,
        "session_id": "claude__reader-e5",
        "agent_kind": "claude_code"
    })
    .to_string();
    let metadata_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", metadata.as_bytes())
            .expect("write chunked fixture metadata")
            .to_string();

    let chunks = libra::internal::ai::history::chunk_transcript_line_safe(transcript, 256)
        .expect("split deterministic chunked reader fixture");
    assert!(chunks.len() > 1, "fixture must contain multiple chunks");
    let mut part_tree_entries = Vec::with_capacity(chunks.len());
    let mut manifest_parts = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        let name = format!("claude_code.jsonl.{:03}", index + 1);
        let oid = libra::utils::object::write_git_object(&libra_dir, "blob", chunk)
            .expect("write chunked fixture transcript part")
            .to_string();
        part_tree_entries.push(("100644".to_string(), name.clone(), oid.clone()));
        manifest_parts.push(json!({
            "path": format!("transcript/{name}"),
            "oid": oid,
            "byte_len": chunk.len(),
        }));
    }
    let part_tree_refs = part_tree_entries
        .iter()
        .map(|(mode, name, oid)| (mode.as_str(), name.as_str(), oid.as_str()))
        .collect::<Vec<_>>();
    let transcript_tree = write_tree(&libra_dir, &part_tree_refs);
    let content_hash = format!("sha256:{}", "a".repeat(64));
    let content_hash_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", content_hash.as_bytes())
            .expect("write chunked fixture content hash")
            .to_string();
    let manifest = json!({
        "schema_version": 1,
        "entries": {
            "metadata": {
                "path": "metadata.json",
                "oid": metadata_oid.clone(),
                "byte_len": metadata.len(),
            },
            "transcript": {
                "path": "transcript/claude_code.jsonl",
                "chunked": true,
                "parts": manifest_parts,
                "byte_len": transcript.len(),
            }
        }
    })
    .to_string();
    let manifest_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", manifest.as_bytes())
            .expect("write chunked fixture manifest")
            .to_string();
    let inner_tree = write_tree(
        &libra_dir,
        &[
            ("100644", "content_hash.txt", &content_hash_oid),
            ("100644", "manifest.json", &manifest_oid),
            ("100644", "metadata.json", &metadata_oid),
            ("40000", "transcript", &transcript_tree),
        ],
    );
    let prefix_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[2..], &inner_tree)]);
    let checkpoint_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[..2], &prefix_tree)]);
    let root_tree = write_tree(&libra_dir, &[("40000", "checkpoint", &checkpoint_tree)]);

    let conn = repo.db().await;
    seed_session(&conn, "claude__reader-e5", 1).await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, 'claude__reader-e5', 'committed', NULL, ?, ?, 'fixture-commit', 1)",
        [checkpoint_id.into(), root_tree.into(), metadata_oid.into()],
    ))
    .await
    .expect("seed chunked checkpoint catalog row");
    checkpoint_id.to_string()
}

/// `checkpoint show` does not turn manifest-derived layout details or
/// metadata contents into a public JSON contract, even for a chunked
/// checkpoint.
#[tokio::test]
async fn chunked_show_keeps_safe_summary_json_contract() {
    // ~40 bytes per line × 40 lines ≈ 1.5 KiB; threshold 256 → ≥ 6 parts.
    let mut transcript = Vec::new();
    for index in 0..40 {
        transcript.extend_from_slice(
            format!("{{\"turn\":{index:04},\"text\":\"chunk me\"}}\n").as_bytes(),
        );
    }
    let repo = ReaderRepo::init();
    let checkpoint_id = seed_chunked_e4_checkpoint(&repo, &transcript).await;

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
        &[],
    );
    let data = json_data(&show);
    assert_eq!(data["checkpoint"]["checkpoint_id"], json!(checkpoint_id));
    assert!(
        data.get("metadata").is_none(),
        "metadata must remain outside default show JSON: {data}"
    );
    assert!(
        data.get("layout").is_none(),
        "manifest paths, object IDs, and hashes must not become default show JSON: {data}"
    );
    assert!(
        !data.to_string().contains("chunk me"),
        "show must not expose transcript content"
    );
}

/// Single-file E4-libra checkpoint: `show` exposes only the fixed safe
/// summary, not metadata or transcript-derived layout details.
#[test]
fn show_does_not_expose_transcript_layout() {
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
    assert!(
        data.get("metadata").is_none(),
        "metadata.json must not be exposed by default show"
    );
    assert!(
        data.get("layout").is_none(),
        "default show JSON must remain schema-stable: {data}"
    );
    assert!(
        !data.to_string().contains("kick off") && !data.to_string().contains("done"),
        "show must not expose transcript body"
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
