//! plan-20260713「本机 live agent 执行验证门」— real local-CLI data tests.
//!
//! Gated twice (L2/L3 tier, GC-DR-07-compatible): the `test-live-agent`
//! Cargo feature keeps these out of `cargo test --all`, and the
//! `LIBRA_RUN_LIVE_AGENT_GATE=1` env keeps a feature-enabled build from
//! touching the developer's real provider stores unless acceptance
//! explicitly opts in. Missing stores print "skipped" and never fail.
//!
//! M2 scope: real BY-ID lookups against the developer machine's actual
//! `~/.claude/projects` (DR-02) and `~/.codex/sessions` (DR-03) stores.

use std::path::{Path, PathBuf};

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
        opencode_export::{ExportLimits, run_export_subprocess_sandboxed, trusted_opencode_binary},
        record_trust,
    };

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

/// Pull one session id out of the real opencode SQLite store without adding
/// a rusqlite dev-dependency: shell out to the `sqlite3` binary when
/// present, else skip.
fn rusqlite_less_query(db: &Path) -> Option<String> {
    // Prefer the sqlite3 CLI; fall back to python3's stdlib sqlite3 (one of
    // the two is present on any dev acceptance machine).
    let try_cmd = |program: &str, args: &[&std::ffi::OsStr]| -> Option<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sid.is_empty()).then_some(sid)
    };
    let sql = "SELECT id FROM session ORDER BY rowid DESC LIMIT 1;";
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
