//! plan-20260713「本机 live agent 执行验证门」— real local-CLI data tests.
//!
//! Gated twice (L2/L3 tier, GC-DR-07-compatible): the `test-live-agent`
//! Cargo feature keeps these out of `cargo test --all`, and the
//! `LIBRA_RUN_LIVE_AGENT_GATE=1` env keeps a feature-enabled build from
//! touching the developer's real provider stores unless acceptance
//! explicitly opts in. The earlier read-only probes may print "skipped" when
//! a store is absent; the M4 three-provider acceptance test is fail-closed in
//! gated mode, so a missing required provider cannot be counted as a pass.
//!
//! M2 scope: real BY-ID lookups against the developer machine's actual
//! `~/.claude/projects` (DR-02) and `~/.codex/sessions` (DR-03) stores. M4
//! adds real three-provider import/idempotency, cross-repo, and erase/restore.
//! M5 adds real Claude subagent-file content attribution and a fail-closed
//! assertion that a real Codex native hook produced boundary evidence.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use libra::internal::ai::observed_agents::{
    claude_project_slug, find_codex_rollout, resolve_session_file,
};

fn gate_enabled() -> bool {
    std::env::var("LIBRA_RUN_LIVE_AGENT_GATE").map(|v| v == "1") == Ok(true)
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// DR-02 live: pick a real session id from this repo's real Claude project
/// dir and resolve it BY ID through `resolve_session_file`.
#[test]
fn live_claude_session_resolves_by_id() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(project_dir) = home()
        .map(|h| {
            h.join(".claude/projects")
                .join(claude_project_slug(repo_root))
        })
        .filter(|d| d.is_dir())
    else {
        eprintln!("skipped (no real ~/.claude project dir for this repo)");
        return;
    };
    let Some(sid) = std::fs::read_dir(&project_dir).ok().and_then(|entries| {
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".jsonl").map(str::to_string)
            })
            .find(|stem| {
                stem.len() == 36 && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            })
    }) else {
        eprintln!("skipped (no real Claude session JSONL found)");
        return;
    };
    let found = resolve_session_file(repo_root, &sid)
        .expect("live by-id lookup must not error")
        .expect("live by-id lookup must find the session");
    assert!(found.ends_with(format!("{sid}.jsonl")));
    eprintln!("live claude by-id lookup ok (session id len {})", sid.len());
}

/// DR-03 live: extract a real session id from a real rollout filename and
/// find it BY ID through `find_codex_rollout`.
#[test]
fn live_codex_rollout_resolves_by_id() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }
    let Some(sessions) = home()
        .map(|h| h.join(".codex/sessions"))
        .filter(|d| d.is_dir())
    else {
        eprintln!("skipped (no real ~/.codex/sessions store)");
        return;
    };
    // Find any real rollout file (bounded manual walk, newest year first).
    fn find_any_rollout(root: &Path, depth: usize) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(root)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        for entry in entries.into_iter().take(64) {
            if depth < 3 && entry.is_dir() {
                if let Some(found) = find_any_rollout(&entry, depth + 1) {
                    return Some(found);
                }
            } else if depth == 3
                && entry
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            {
                return Some(entry);
            }
        }
        None
    }
    let Some(rollout) = find_any_rollout(&sessions, 0) else {
        eprintln!("skipped (no real Codex rollout file found)");
        return;
    };
    let name = rollout.file_name().unwrap().to_string_lossy().into_owned();
    let stem = name.strip_suffix(".jsonl").unwrap_or(&name);
    // Session id = trailing UUID (36 chars) of the rollout filename.
    let sid: String = stem
        .chars()
        .skip(stem.chars().count().saturating_sub(36))
        .collect();
    if sid.len() != 36 || !sid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        eprintln!("skipped (rollout filename shape unexpected: cannot extract session id)");
        return;
    }
    let found = find_codex_rollout(&sid)
        .expect("live by-id lookup must not error")
        .expect("live by-id lookup must find a rollout");
    assert!(
        found
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(&format!("-{sid}.jsonl"))),
        "found rollout must carry the session id"
    );
    eprintln!("live codex by-id lookup ok (session id len {})", sid.len());
}

/// DR-04b live (M3): trust the REAL local `opencode` binary (operator-grade
/// registration: trusted dir + provenance record), then run a REAL
/// `opencode export` of a REAL session under the Required bwrap offline
/// profile and normalize it through coverage-v1. Skips when the store or
/// binary is absent.
#[tokio::test]
async fn live_opencode_sandboxed_export_normalizes_real_session() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }
    use libra::internal::ai::observed_agents::{
        add_trusted_dir, normalize_opencode_export,
        opencode_export::{
            ExportLimits, run_export_subprocess_sandboxed, trusted_bwrap_available,
            trusted_opencode_binary,
        },
        record_trust,
    };

    if !trusted_bwrap_available() {
        eprintln!("skipped (trusted bwrap cannot create the required namespaces)");
        return;
    }

    let Some(binary) = home()
        .map(|h| h.join(".opencode/bin/opencode"))
        .filter(|p| p.is_file())
    else {
        eprintln!("skipped (no real ~/.opencode/bin/opencode)");
        return;
    };
    let Some(db) = home()
        .map(|h| h.join(".local/share/opencode/opencode.db"))
        .filter(|p| p.is_file())
    else {
        eprintln!("skipped (no real opencode session store)");
        return;
    };
    // A real session id straight from the real store.
    let sid = {
        let conn = rusqlite_less_query(&db);
        match conn {
            Some(sid) => sid,
            None => {
                eprintln!("skipped (no session rows in the real opencode store)");
                return;
            }
        }
    };

    // Operator-grade trust registration for the real binary (idempotent;
    // exactly what the plan expects the acceptance machine to do).
    let dir = binary.parent().expect("binary has a parent");
    add_trusted_dir(dir).await.expect("register trusted dir");
    record_trust("opencode", &binary)
        .await
        .expect("record opencode trust");
    let trusted = trusted_opencode_binary()
        .await
        .expect("trusted binary resolves");

    let bytes = run_export_subprocess_sandboxed(&trusted, &sid, ExportLimits::default())
        .await
        .expect("real sandboxed export must succeed offline");
    assert!(!bytes.is_empty());
    let turns = normalize_opencode_export(&bytes);
    assert!(
        !turns.is_empty(),
        "a real session must normalize to at least one turn"
    );
    eprintln!(
        "live opencode sandboxed export ok ({} bytes, {} turns)",
        bytes.len(),
        turns.len()
    );
}

/// M4 live acceptance: import one real current-repository session from every
/// delivered provider path, prove replay is a per-turn no-op, reject one real
/// cross-repository Claude source, then exercise erase → blocked replay →
/// audited restore on a Claude session that was not captured before this test.
///
/// This intentionally mutates only `refs/libra/traces` and the capture catalog
/// of the checkout whose operator explicitly enabled the live gate. It never
/// edits provider stores or working-tree files.
#[tokio::test]
async fn live_m4_historical_import_three_provider_acceptance() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }

    use libra::{
        internal::{
            ai::{
                history::HistoryManager,
                observed_agents::{
                    add_trusted_dir,
                    opencode_export::{trusted_bwrap_available, trusted_opencode_binary},
                    record_trust,
                },
            },
            branch::TRACES_BRANCH,
        },
        utils::client_storage::ClientStorage,
    };
    use sea_orm::{ConnectionTrait, Database, Statement};

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = home().expect("live M4 gate requires a home directory");
    let claude_dir = home
        .join(".claude/projects")
        .join(claude_project_slug(repo));
    let mut claude_candidates = jsonl_candidates(&claude_dir, "claude_code", repo);
    claude_candidates.sort_by_key(|(_, path)| path.metadata().map(|m| m.len()).unwrap_or(u64::MAX));
    assert!(
        !claude_candidates.is_empty(),
        "live M4 gate requires a real Claude session for this repository"
    );

    let db_url = format!("sqlite://{}", repo.join(".libra/libra.db").display());
    let db = Database::connect(&db_url)
        .await
        .expect("open current repository capture database");
    let gate_owned_claude = std::env::var("LIBRA_LIVE_M4_GATE_OWNED_CLAUDE_SESSION").ok();
    let mut selected_claude = None;
    for (sid, path) in &claude_candidates {
        let explicitly_gate_owned = gate_owned_claude.as_deref() == Some(sid.as_str());
        let existing = db
            .query_one_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT 1 AS one FROM agent_session \
                 WHERE agent_kind = 'claude_code' AND provider_session_id = ?",
                [sid.clone().into()],
            ))
            .await
            .expect("query pre-existing Claude capture")
            .is_some();
        let selected = if gate_owned_claude.is_some() {
            explicitly_gate_owned
        } else {
            !existing
        };
        if selected {
            selected_claude = Some((sid.clone(), path.clone()));
            break;
        }
    }
    let (claude_sid, _claude_path) = selected_claude.expect(
        "live M4 gate needs one uncaptured Claude session, or an explicit \
         LIBRA_LIVE_M4_GATE_OWNED_CLAUDE_SESSION, so erase/restore cannot delete unrelated data",
    );

    if gate_owned_claude.is_some() {
        let tombstone_exists = db
            .query_one_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT 1 AS one FROM agent_import_tombstone \
                 WHERE agent_kind = 'claude_code' AND provider_session_id = ?",
                [claude_sid.clone().into()],
            ))
            .await
            .expect("query gate-owned Claude tombstone")
            .is_some();
        if tombstone_exists {
            let libra_dir = repo.join(".libra");
            let history = HistoryManager::new_with_ref(
                Arc::new(ClientStorage::init(libra_dir.join("objects"))),
                libra_dir,
                Arc::new(db.clone()),
                TRACES_BRANCH,
            );
            history
                .erase_session_local(&format!("claude__{claude_sid}"))
                .await
                .expect("finish interrupted erasure for the gate-owned Claude capture");
            drop(history);
            let restored = run_live_import(repo, "claude-code", "--session", &claude_sid, true);
            if restored.status.success() {
                // The audited restore completed in one pass.
            } else {
                assert!(
                    String::from_utf8_lossy(&restored.stderr).contains("LBR-AGENT-018"),
                    "restore interrupted gate-owned Claude capture: {}",
                    describe_output(&restored)
                );
                let repaired =
                    run_live_import(repo, "claude-code", "--session", &claude_sid, false);
                assert!(
                    repaired.status.success(),
                    "repair restored gate-owned Claude capture: {}",
                    describe_output(&repaired)
                );
            }
        }
    }

    let codex_sessions = home.join(".codex/sessions");
    let (codex_sid, _) = find_codex_session_for_repo(&codex_sessions, repo, 0)
        .expect("live M4 gate requires a real Codex rollout for this repository");

    assert!(
        trusted_bwrap_available(),
        "live M4 gate requires a trusted bwrap with usable namespaces"
    );
    let opencode_binary = home.join(".opencode/bin/opencode");
    assert!(
        opencode_binary.is_file(),
        "live M4 gate requires the real OpenCode binary"
    );
    add_trusted_dir(
        opencode_binary
            .parent()
            .expect("OpenCode binary has a parent"),
    )
    .await
    .expect("register real OpenCode directory");
    record_trust("opencode", &opencode_binary)
        .await
        .expect("record real OpenCode trust");
    trusted_opencode_binary()
        .await
        .expect("trusted OpenCode binary resolves");
    let opencode_db = home.join(".local/share/opencode/opencode.db");
    let opencode_sid = opencode_session_for_repo(&opencode_db, repo)
        .expect("live M4 gate requires a real OpenCode session for this repository");

    for (agent, sid) in [
        ("claude-code", claude_sid.as_str()),
        ("codex", codex_sid.as_str()),
        ("opencode", opencode_sid.as_str()),
    ] {
        let first = run_live_import(repo, agent, "--session", sid, false);
        assert!(
            first.status.success(),
            "real {agent} import failed: {}",
            describe_output(&first)
        );
        let replay = run_live_import(repo, agent, "--session", sid, false);
        assert!(
            replay.status.success(),
            "real {agent} replay failed: {}",
            describe_output(&replay)
        );
        assert_eq!(
            imported_checkpoint_count(&replay),
            0,
            "real {agent} replay must be a coverage no-op"
        );
    }

    let cross_repo = find_cross_repo_claude_source(&home.join(".claude/projects"), repo)
        .expect("live M4 gate requires one real cross-repository Claude source");
    let cross = run_live_import(
        repo,
        "claude-code",
        "--path",
        cross_repo.to_string_lossy().as_ref(),
        false,
    );
    assert!(
        !cross.status.success(),
        "cross-repository import unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&cross.stderr).contains("LBR-AGENT-015"),
        "cross-repository rejection lost its stable code: {}",
        describe_output(&cross)
    );

    let libra_dir = repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(db),
        TRACES_BRANCH,
    );
    let erased = history
        .erase_session_local(&format!("claude__{claude_sid}"))
        .await
        .expect("erase the gate-owned Claude capture");
    assert!(erased.session_deleted);
    drop(history);

    let blocked = run_live_import(repo, "claude-code", "--session", &claude_sid, false);
    assert!(
        !blocked.status.success(),
        "erased real session was resurrected"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("LBR-AGENT-019"),
        "erased replay lost its stable code: {}",
        describe_output(&blocked)
    );
    let restored = run_live_import(repo, "claude-code", "--session", &claude_sid, true);
    if !restored.status.success() {
        assert!(
            String::from_utf8_lossy(&restored.stderr).contains("LBR-AGENT-018"),
            "audited real-session restore failed: {}",
            describe_output(&restored)
        );
        let repaired = run_live_import(repo, "claude-code", "--session", &claude_sid, false);
        assert!(
            repaired.status.success(),
            "audited real-session restore repair failed: {}",
            describe_output(&repaired)
        );
    }
    let restored_replay = run_live_import(repo, "claude-code", "--session", &claude_sid, false);
    assert!(restored_replay.status.success());
    assert_eq!(imported_checkpoint_count(&restored_replay), 0);

    eprintln!(
        "live M4 import gate ok (claude/codex/opencode replay no-op, cross-repo rejected, erase/restore fenced)"
    );
}

/// M5 / DR-06 live acceptance. Replaying a real, already-captured Claude
/// session with real `<session>/subagents/*.jsonl` sources must materialize one
/// current content leaf per file, keep every Claude link unresolved, and add
/// no revision on replay. A real Codex native subagent hook must also have
/// produced at least one distinct boundary checkpoint in this repository.
#[tokio::test]
async fn live_m5_subagent_boundary_content_attribution() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }
    use sea_orm::{ConnectionTrait, Database, Statement};

    const HARD_CAP: u64 = 16 * 1024 * 1024;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = home().expect("live M5 gate requires a home directory");
    let project_dir = home
        .join(".claude/projects")
        .join(claude_project_slug(repo));
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&project_dir)
        .expect("live M5 gate requires the real Claude project directory")
        .filter_map(Result::ok)
    {
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let Some(session_id) = session_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if session_id.len() != 36
            || !session_id
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            continue;
        }
        let parent = project_dir.join(format!("{session_id}.jsonl"));
        let subagents = session_dir.join("subagents");
        if !parent.is_file() || !subagents.is_dir() {
            continue;
        }
        let child_files = std::fs::read_dir(&subagents)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect::<Vec<_>>();
        let child_bytes = child_files
            .iter()
            .filter_map(|path| path.metadata().ok().map(|metadata| metadata.len()))
            .sum::<u64>();
        let parent_bytes = parent
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(u64::MAX);
        if !child_files.is_empty() && parent_bytes <= HARD_CAP && child_bytes <= HARD_CAP {
            candidates.push((
                session_id.to_string(),
                child_files.len(),
                parent_bytes.saturating_add(child_bytes),
            ));
        }
    }
    candidates.sort_by_key(|(_, _, bytes)| *bytes);
    let (claude_sid, expected_sources, _) = candidates
        .first()
        .cloned()
        .expect("live M5 gate requires a real bounded Claude session with subagent files");

    let first = run_live_import(repo, "claude-code", "--session", &claude_sid, false);
    assert!(
        first.status.success(),
        "real Claude subagent import failed: {}",
        describe_output(&first)
    );
    let db = Database::connect(format!(
        "sqlite://{}",
        repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open M5 live capture database");
    let parent_session_id = format!("claude__{claude_sid}");
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT c.current_revision, c.source_key, cp.parent_checkpoint_id,
                    l.link_state, l.boundary_checkpoint_id
             FROM agent_subagent_content_claim c
             JOIN agent_checkpoint cp ON cp.checkpoint_id = c.current_checkpoint_id
             JOIN agent_subagent_link l ON l.content_checkpoint_id = c.current_checkpoint_id
             WHERE c.parent_session_id = ? AND c.provider_kind = 'claude_code'
             ORDER BY c.source_key",
            [parent_session_id.clone().into()],
        ))
        .await
        .expect("read live Claude subagent attribution");
    assert_eq!(rows.len(), expected_sources);
    for row in &rows {
        assert!(
            row.try_get_by::<i64, _>("current_revision")
                .expect("current revision")
                >= 1
        );
        let source_key: String = row.try_get_by("source_key").expect("source key");
        assert!(!Path::new(&source_key).is_absolute());
        assert!(source_key.starts_with("source/sha256/"));
        assert_eq!(source_key.len(), "source/sha256/".len() + 64);
        assert!(!source_key.contains(&claude_sid));
        assert_eq!(
            row.try_get_by::<Option<String>, _>("parent_checkpoint_id")
                .expect("structural parent"),
            None
        );
        assert_eq!(
            row.try_get_by::<String, _>("link_state")
                .expect("link state"),
            "unresolved"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                .expect("boundary link"),
            None
        );
    }
    let before_replay_revisions = subagent_revision_count(&db, &parent_session_id).await;
    let replay = run_live_import(repo, "claude-code", "--session", &claude_sid, false);
    assert!(
        replay.status.success(),
        "real Claude subagent replay failed: {}",
        describe_output(&replay)
    );
    assert_eq!(
        subagent_revision_count(&db, &parent_session_id).await,
        before_replay_revisions,
        "real Claude subagent replay must not append revisions"
    );

    let codex_boundaries = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) AS n
             FROM agent_checkpoint cp
             JOIN agent_session s ON s.session_id = cp.session_id
             LEFT JOIN agent_subagent_link l ON l.content_checkpoint_id = cp.checkpoint_id
             WHERE s.agent_kind = 'codex' AND cp.scope = 'subagent'
               AND l.content_checkpoint_id IS NULL"
                .to_string(),
        ))
        .await
        .expect("read live Codex boundary count")
        .expect("Codex boundary count row")
        .try_get_by::<i64, _>("n")
        .expect("Codex boundary count");
    assert!(
        codex_boundaries > 0,
        "live M5 gate requires a real Codex native SubagentStart/Stop boundary capture"
    );

    eprintln!(
        "live M5 subagent gate ok (Claude content sources/replay/unresolved, Codex native boundary)"
    );
}

/// M6 / DR-07 live acceptance. Render a real indexed capture through the
/// public JSON command, prove non-TTY calls fail before TUI initialization,
/// and compare every capture/import/export catalog row before and after.
///
/// Erased-state rendering is covered by the deterministic L1 fixture because
/// this gate must not erase an operator-owned live capture merely to manufacture
/// a tombstone. If a prior gate-owned erased tombstone exists, it is also read
/// through the public command here.
#[tokio::test]
async fn live_m6_agent_graph_real_capture_is_private_and_readonly() {
    if !gate_enabled() {
        eprintln!("skipped (set LIBRA_RUN_LIVE_AGENT_GATE=1 for the live agent gate)");
        return;
    }
    use sea_orm::{ConnectionTrait, Database, Statement};

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let db = Database::connect(format!(
        "sqlite://{}",
        repo.join(".libra/libra.db").display()
    ))
    .await
    .expect("open M6 live capture database");
    let session_id = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT s.session_id
             FROM agent_session s
             WHERE EXISTS (
                 SELECT 1 FROM agent_coverage_claim c
                 WHERE c.session_id = s.session_id AND c.state = 'catalog_committed'
             )
             ORDER BY s.last_event_at DESC, s.session_id
             LIMIT 1"
                .to_string(),
        ))
        .await
        .expect("query a real indexed capture")
        .expect("live M6 gate requires a real indexed captured session")
        .try_get_by::<String, _>("session_id")
        .expect("real capture session id");

    let before = live_capture_snapshot(&db).await;
    let json = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(repo)
        .args(["--json", "agent", "graph", &session_id])
        .output()
        .expect("run live agent graph JSON");
    assert!(
        json.status.success(),
        "real capture graph failed: {}",
        describe_output(&json)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("real graph stdout is JSON");
    let data = &envelope["data"];
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["state"], "present");
    assert_eq!(data["session"]["session_id"], session_id);
    assert!(
        !data["turns"]
            .as_array()
            .expect("real graph turns")
            .is_empty(),
        "real indexed capture must expose at least one turn"
    );

    let stdout = String::from_utf8_lossy(&json.stdout);
    for forbidden in [
        "working_dir",
        "metadata_json",
        "description",
        "redaction_report",
        "coverage_digest",
        "tree_oid",
        "metadata_blob_oid",
        "traces_commit",
        "parent_commit",
        repo.to_string_lossy().as_ref(),
    ] {
        assert!(
            !stdout.contains(forbidden),
            "real capture graph leaked forbidden field/value `{forbidden}`"
        );
    }

    let non_tty = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(repo)
        .args(["agent", "graph", &session_id])
        .output()
        .expect("run live non-TTY graph");
    assert_eq!(non_tty.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&non_tty.stderr).contains("LBR-CLI-002"));

    let erased_session_id = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT erased_session_id
             FROM agent_import_tombstone
             WHERE erased_session_id IS NOT NULL
             ORDER BY erased_at DESC
             LIMIT 1"
                .to_string(),
        ))
        .await
        .expect("query optional gate-owned erased tombstone")
        .and_then(|row| row.try_get_by::<String, _>("erased_session_id").ok());
    if let Some(erased_session_id) = erased_session_id {
        let erased = Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(repo)
            .args(["--json", "agent", "graph", &erased_session_id])
            .output()
            .expect("render live erased graph");
        assert!(erased.status.success(), "{}", describe_output(&erased));
        let erased: serde_json::Value =
            serde_json::from_slice(&erased.stdout).expect("erased graph stdout is JSON");
        assert_eq!(erased["data"]["state"], "erased");
        assert!(erased["data"]["session"].is_null());
        assert_eq!(erased["data"]["turns"], serde_json::json!([]));
    }

    let after = live_capture_snapshot(&db).await;
    assert_eq!(
        before, after,
        "agent graph changed a live capture/import/export catalog row"
    );
    eprintln!(
        "live M6 graph gate ok (real indexed JSON/private/non-TTY/zero-write; erased state independently pinned by L1)"
    );
}

async fn live_capture_snapshot(
    connection: &sea_orm::DatabaseConnection,
) -> BTreeMap<String, String> {
    use sea_orm::{ConnectionTrait, Statement};

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
            .expect("read live capture table columns")
            .into_iter()
            .map(|row| row.try_get_by::<String, _>("name").expect("column name"))
            .collect::<Vec<_>>();
        let signature = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || char(31) || ");
        let row = connection
            .query_one_raw(Statement::from_string(
                connection.get_database_backend(),
                format!(
                    "SELECT COALESCE(group_concat(row_signature, char(30)), '') AS snapshot
                     FROM (SELECT {signature} AS row_signature FROM {table} ORDER BY row_signature)"
                ),
            ))
            .await
            .expect("snapshot live capture table")
            .expect("snapshot aggregate row");
        snapshot.insert(
            table.to_string(),
            row.try_get_by("snapshot").expect("snapshot string"),
        );
    }
    snapshot
}

async fn subagent_revision_count(conn: &sea_orm::DatabaseConnection, session_id: &str) -> i64 {
    use sea_orm::{ConnectionTrait, Statement};

    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
         WHERE parent_session_id = ?",
        [session_id.into()],
    ))
    .await
    .expect("query subagent revision count")
    .expect("subagent revision count row")
    .try_get_by("n")
    .expect("subagent revision count")
}

fn run_live_import(
    repo: &Path,
    agent: &str,
    selector: &str,
    value: &str,
    restore_erased: bool,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command.current_dir(repo).args([
        "agent", "import", selector, value, "--agent", agent, "--yes", "--json",
    ]);
    if restore_erased {
        command.arg("--restore-erased");
    }
    command.output().expect("run live historical import")
}

fn imported_checkpoint_count(output: &Output) -> u64 {
    serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("live import stdout is JSON")
        ["data"]["results"][0]["checkpoints_written"]
        .as_u64()
        .expect("live import summary has checkpoints_written")
}

fn describe_output(output: &Output) -> String {
    format!(
        "status={:?}, stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn jsonl_candidates(root: &Path, kind: &str, repo: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| jsonl_identity(&path, kind).map(|(sid, cwd)| (sid, cwd, path)))
        .filter(|(_, cwd, _)| same_canonical_path(cwd, repo))
        .map(|(sid, _, path)| (sid, path))
        .collect()
}

fn jsonl_identity(path: &Path, kind: &str) -> Option<(String, PathBuf)> {
    let reader = BufReader::new(std::fs::File::open(path).ok()?);
    for line in reader.lines().take(256).filter_map(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if kind == "codex" && value["type"] == "session_meta" {
            let sid = value["payload"]["id"].as_str()?.to_string();
            let cwd = PathBuf::from(value["payload"]["cwd"].as_str()?);
            return Some((sid, cwd));
        }
        if kind == "claude_code"
            && let (Some(sid), Some(cwd)) = (value["sessionId"].as_str(), value["cwd"].as_str())
        {
            return Some((sid.to_string(), PathBuf::from(cwd)));
        }
    }
    None
}

fn find_codex_session_for_repo(
    root: &Path,
    repo: &Path,
    depth: usize,
) -> Option<(String, PathBuf)> {
    let mut entries = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|a, b| b.cmp(a));
    for path in entries.into_iter().take(256) {
        if path.is_dir() && depth < 3 {
            if let Some(found) = find_codex_session_for_repo(&path, repo, depth + 1) {
                return Some(found);
            }
        } else if depth == 3
            && path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some((sid, cwd)) = jsonl_identity(&path, "codex")
            && same_canonical_path(&cwd, repo)
            && codex_source_has_one_matching_identity(&path, &sid)
        {
            return Some((sid, path));
        }
    }
    None
}

fn codex_source_has_one_matching_identity(path: &Path, selected: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut identities = std::collections::BTreeSet::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !line.contains("\"type\":\"session_meta\"")
            && !line.contains("\"type\": \"session_meta\"")
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            return false;
        };
        if value["type"] == "session_meta"
            && let Some(id) = value["payload"]["id"].as_str()
        {
            identities.insert(id.to_string());
            if identities.len() > 1 {
                return false;
            }
        }
    }
    identities.len() == 1 && identities.contains(selected)
}

fn same_canonical_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left.starts_with(right),
        _ => false,
    }
}

fn find_cross_repo_claude_source(projects: &Path, repo: &Path) -> Option<PathBuf> {
    let mut project_dirs = std::fs::read_dir(projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    project_dirs.sort();
    for project_dir in project_dirs {
        for (sid, path) in jsonl_candidates_any_repo(&project_dir) {
            let Some((parsed_sid, cwd)) = jsonl_identity(&path, "claude_code") else {
                continue;
            };
            if sid == parsed_sid && cwd.exists() && !same_canonical_path(&cwd, repo) {
                return Some(path);
            }
        }
    }
    None
}

fn jsonl_candidates_any_repo(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| jsonl_identity(&path, "claude_code").map(|(sid, _)| (sid, path)))
        .collect()
}

fn opencode_session_for_repo(db: &Path, repo: &Path) -> Option<String> {
    let escaped = repo.to_string_lossy().replace('\'', "''");
    let sql = format!(
        "SELECT id FROM session WHERE directory = '{escaped}' ORDER BY time_updated DESC LIMIT 1;"
    );
    query_single(db, &sql)
}

/// Pull one session id out of the real opencode SQLite store without adding
/// a rusqlite dev-dependency: shell out to the `sqlite3` binary when
/// present, else skip.
fn rusqlite_less_query(db: &Path) -> Option<String> {
    // Prefer the sqlite3 CLI; fall back to python3's stdlib sqlite3 (one of
    // the two is present on any dev acceptance machine).
    query_single(db, "SELECT id FROM session ORDER BY rowid DESC LIMIT 1;")
}

fn query_single(db: &Path, sql: &str) -> Option<String> {
    let try_cmd = |program: &str, args: &[&std::ffi::OsStr]| -> Option<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sid.is_empty()).then_some(sid)
    };
    try_cmd("sqlite3", &[db.as_os_str(), std::ffi::OsStr::new(sql)]).or_else(|| {
        let script = format!(
            "import sqlite3;print(sqlite3.connect({:?}).execute({sql:?}).fetchone()[0])",
            db.display().to_string()
        );
        try_cmd(
            "python3",
            &[std::ffi::OsStr::new("-c"), std::ffi::OsStr::new(&script)],
        )
    })
}
