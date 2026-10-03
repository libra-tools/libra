//! Task A6.5 — Agent 第一期本地三 Agent 采集 smoke (plan.md §0.3, first-batch
//! hard gate).
//!
//! Drives the **real**, locally installed `codex`, `claude` and `opencode`
//! CLIs (one minimal paid session each) in throwaway Libra repositories
//! and asserts the whole capture chain end to end: `libra agent add` hook
//! install (pinned-binary provenance + user-config preservation) → real
//! non-interactive session → `agent session/checkpoint list`,
//! safe-summary `checkpoint show`, `session show`, `refs/libra/traces`,
//! `agent doctor` → §0.3.5 uninstall smoke (semantic config restore,
//! idempotent remove, captured data retained). Deterministic fixtures
//! cannot stand in for this target (plan.md A6.5 acceptance).
//!
//! CI-safe by construction: every test is `#[ignore]` **and** skips unless
//! `LIBRA_RUN_LOCAL_AGENTS=1`, so neither `cargo test --all` nor a stray
//! `--ignored` run can start a paid session. Tests are `#[serial]` and the
//! canonical invocation adds `--test-threads=1` (§0.3.6):
//!
//! ```bash
//! LIBRA_RUN_LOCAL_AGENTS=1 \
//! LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode \
//! cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
//! ```
//!
//! Knobs (plan.md §0.3.6): `LIBRA_LOCAL_AGENT_SET` (default all three),
//! `LIBRA_LOCAL_AGENT_TIMEOUT_SECS` (default 180; expiry kills the
//! child's whole **process group**), `LIBRA_KEEP_LOCAL_AGENT_SMOKE=1`
//! (keep evidence — sensitive, never commit),
//! `LIBRA_LOCAL_AGENT_EVIDENCE_DIR` (custom 0700 evidence root).
//!
//! A missing binary or a failed read-only login probe marks that agent's
//! run **BLOCKED** without starting a paid session (§0 blocked rule) and
//! **fails the test with a `BLOCKED` panic**: with the gate explicitly
//! open, blocked stays machine-distinguishable (exit code / pass count)
//! from a real green run and can never be faked green. The shared driver — and
//! the documented per-CLI isolation decisions (isolated `CODEX_HOME` for
//! codex; real `HOME` + project-local capture configs for claude and
//! opencode) — lives in `tests/harness/agent_local_capture.rs`.

#![cfg(unix)]

mod harness;

use harness::agent_local_capture as smoke;
use serial_test::serial;

/// Guard against `LIBRA_LOCAL_AGENT_SET` typos silently skipping an agent
/// (checked in the gated path only — unknown slugs are a hard error).
fn check_agent_set() {
    if std::env::var(smoke::GATE_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        let unknown = smoke::unknown_requested_agents();
        assert!(
            unknown.is_empty(),
            "unknown agent slug(s) in {}: {unknown:?} (known: codex, claude-code, opencode)",
            smoke::SET_ENV
        );
    }
}

#[test]
#[ignore = "drives a real paid codex session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_codex() {
    check_agent_set();
    smoke::run_slug("codex");
}

#[test]
#[ignore = "drives a real paid claude session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_claude_code() {
    check_agent_set();
    smoke::run_slug("claude-code");
}

#[test]
#[ignore = "drives a real paid opencode session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_opencode() {
    check_agent_set();
    smoke::run_slug("opencode");
}

/// OpenCode's A6.5 criteria remain lifecycle-only on every platform.
/// macOS content export is rejected before spawn; this source guard keeps the
/// smoke driver from reviving a seatbelt content-capture branch. Not live,
/// not `#[ignore]`.
#[test]
fn linux_a65_criteria_unchanged() {
    let harness = include_str!("harness/agent_local_capture.rs");
    assert!(
        harness.contains("on every\n//!   platform pin an empty transcript snapshot with `extraction.present=false`"),
        "A6.5 must retain the lifecycle-only OpenCode snapshot contract on every platform"
    );
    assert!(
        harness.contains("macOS OpenCode export is rejected before spawn"),
        "the smoke driver must document that unsupported macOS export is rejected before spawn"
    );
    assert!(
        !harness.contains("macOS seatbelt export must capture content"),
        "the smoke driver must not revive a macOS seatbelt content-capture branch"
    );
    let smoke = include_str!("agent_local_capture_smoke_test.rs");
    let opencode_smoke_name = ["local_capture", "smoke", "opencode"].join("_");
    let opencode_smoke_signature = ["fn ", &opencode_smoke_name, "("].concat();
    assert!(
        smoke.contains(&opencode_smoke_signature),
        "original A6.5 opencode smoke fn must remain"
    );
    let opencode_run_call = ["smoke::run_slug(", r#""opencode")"#].concat();
    assert!(
        smoke.contains(&opencode_run_call),
        "original A6.5 opencode smoke must still call run_slug"
    );
    let unsupported_macos_smoke = ["fn", "local_capture_smoke_opencode_macos("].join(" ");
    assert!(
        !smoke.contains(&unsupported_macos_smoke),
        "the unsupported macOS export smoke must stay absent"
    );
}
