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
