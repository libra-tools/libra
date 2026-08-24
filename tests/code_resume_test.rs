//! Wave 9 / PR 9 — `libra code --resume <thread_id>` CLI surface
//! coverage (§5.16).
//!
//! What this test pins:
//!   * **Happy path**: a chat-only `libra code` session, after
//!     submitting one user message and shutting down cleanly, can
//!     be resumed by passing the chat-flow session id (from
//!     `generate_session_id`, e.g. `19e…-…-0000`) to `--resume`.
//!     The resumed snapshot must surface the original user message
//!     in its transcript so the runtime is demonstrably restoring
//!     prior history rather than starting empty.
//!   * **Negative — unknown identifier**: passing a syntactically
//!     valid UUID that does not match any persisted session
//!     surfaces the documented "no Libra Code session found …"
//!     error and the process exits non-zero.
//!   * **Negative — non-existent format**: passing a string that
//!     is neither a known UUID nor a known chat session id is
//!     rejected with the same unified "no Libra Code session
//!     found …" error. Wave 9 follow-up dropped the
//!     UUID-only pre-validation in `code.rs`; identifier shape is
//!     now an internal detail of the store, and the CLI only
//!     enforces non-empty input.
//!   * The harness extensions
//!     (`CodeSessionOptions::with_existing_repo_dir`,
//!     `CodeSessionOptions::with_resume_thread`) drive the spawn
//!     with the correct argv shape, and `existing_repo_dir`
//!     guarantees the resume invocation re-uses the same
//!     `working_dir` that scoped the `SessionStore::save`.
//!
//! All cases drive a real, non-TTY `libra code` Web process through
//! `harness::CodeSession::spawn` and capture the resume outcome through
//! both the live HTTP snapshot and the artifact directory the harness
//! writes for every spawn.
//!
#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use std::{path::PathBuf, process::Command, time::Duration};

#[cfg(feature = "test-provider")]
use anyhow::{Context, Result, bail};
#[cfg(feature = "test-provider")]
use harness::{CodeSession, CodeSessionOptions};
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
const FIXTURE: &str = "tests/fixtures/code_ui/basic_chat.json";
#[cfg(feature = "test-provider")]
const DELAYED_FIXTURE: &str = "tests/fixtures/code_ui/delayed_chat.json";

#[cfg(feature = "test-provider")]
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

#[cfg(feature = "test-provider")]
fn delayed_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DELAYED_FIXTURE)
}

#[cfg(feature = "test-provider")]
fn libra_bin_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_libra")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_libra is set for integration tests")
}

#[cfg(feature = "test-provider")]
fn snapshot_status(snapshot: &serde_json::Value) -> Option<&str> {
    snapshot.get("status").and_then(|v| v.as_str())
}

#[cfg(feature = "test-provider")]
fn transcript_contains(snapshot: &serde_json::Value, needle: &str) -> bool {
    snapshot
        .get("transcript")
        .and_then(|v| v.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                let matches = |key: &str| {
                    entry
                        .get(key)
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.contains(needle))
                };
                matches("content") || matches("title")
            })
        })
}

#[cfg(feature = "test-provider")]
fn run_libra_init(repo_dir: &std::path::Path) -> Result<()> {
    let output = Command::new(libra_bin_path())
        .args(["init", "--vault=false", "--quiet"])
        .arg(repo_dir)
        .output()
        .context("failed to run 'libra init'")?;
    if !output.status.success() {
        bail!(
            "libra init failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Spawn `libra code` through the Web process harness with the supplied
/// `--resume <thread_id>` value, expect the spawn to fail
/// because the runtime exits before writing control info, and
/// return the captured process.log + libra.log so the caller can
/// pin the documented failure message.
#[cfg(feature = "test-provider")]
fn expect_resume_spawn_failure(case_name: &str, thread_id: &str) -> Result<(String, String)> {
    let repo_root = tempfile::Builder::new()
        .prefix(&format!("{case_name}-"))
        .tempdir()
        .context("failed to create resume tempdir")?;
    let repo_dir = repo_root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).context("failed to create repo subdir")?;
    run_libra_init(&repo_dir)?;

    let result = CodeSession::spawn(
        CodeSessionOptions::new(case_name, fixture_path())
            .with_existing_repo_dir(repo_dir.clone())
            .with_resume_thread(thread_id),
    );
    match result {
        Ok(_session) => {
            bail!("expected --resume {thread_id} to fail spawn, but a session was created")
        }
        Err(_err) => {}
    }
    let logs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("code-ui-scenarios")
        .join(case_name);
    let process_log = std::fs::read_to_string(logs_dir.join("process.log")).unwrap_or_default();
    let libra_log = std::fs::read_to_string(logs_dir.join("libra.log")).unwrap_or_default();
    Ok((process_log, libra_log))
}

/// Pin the `--resume <uuid>` failure path: a syntactically valid
/// UUID that does not match any persisted session must surface
/// the documented "no Libra Code session found …" error. Driving
/// this from a real non-TTY `libra code` Web process (rather than calling
/// `SessionStore` directly) covers the CLI argv plumbing all the
/// way down to `SessionStore::load_for_thread_id`.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn resume_with_unknown_uuid_thread_id_surfaces_session_not_found() -> Result<()> {
    let phantom_thread_id = "00000000-1111-2222-3333-444455556666";
    let (process, libra) =
        expect_resume_spawn_failure("code-resume-unknown-uuid", phantom_thread_id)?;
    let combined = format!("{process}\n{libra}");
    assert!(
        combined.contains("no Libra Code session found") && combined.contains(phantom_thread_id),
        "combined logs did not include the documented session-not-found error;\nprocess.log:\n{process}\nlibra.log:\n{libra}"
    );
    Ok(())
}

/// Pin the `--resume <unknown-shape>` validation path: any
/// non-existent identifier — UUID-shaped or otherwise — must
/// surface the unified "no Libra Code session found …" error.
/// Wave 9 follow-up dropped the early UUID-only pre-check so the
/// CLI no longer rejects chat-flow session ids before they reach
/// the store; identifier shape is internal and the failure
/// message is the same regardless of which alphabet the input
/// uses.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn resume_with_unknown_non_uuid_thread_id_surfaces_session_not_found() -> Result<()> {
    let bad_thread_id = "not-a-uuid-at-all";
    let (process, libra) = expect_resume_spawn_failure("code-resume-bad-format", bad_thread_id)?;
    let combined = format!("{process}\n{libra}");
    assert!(
        combined.contains("no Libra Code session found") && combined.contains(bad_thread_id),
        "combined logs did not include the unified session-not-found error;\nprocess.log:\n{process}\nlibra.log:\n{libra}"
    );
    Ok(())
}

/// Wave 9 closure — happy-path `--resume <session_id>`.
///
/// 1. Spawn `libra code` against a temp repo with the fake
///    provider basic_chat fixture.
/// 2. Submit a single chat turn so `SessionStore::save` flushes
///    the user message to `<repo>/.libra/sessions/<id>/events.jsonl`.
/// 3. Capture the snapshot's `sessionId` (chat-flow format,
///    `{ts:x}-{pid:x}-{counter:x}`) and shut down cleanly so the
///    `App::leave` fallback save also fires.
/// 4. Re-spawn `libra code --resume <session_id>` against the
///    SAME `existing_repo_dir` (the store filters by working
///    dir) and assert the resumed snapshot's transcript contains
///    the original user message.
///
/// This pins both the runtime change (CLI no longer requires
/// UUID shape) and the end-to-end persistence contract: chat
/// sessions ARE saved across runs with no plan-workflow binding.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn resume_with_chat_session_id_restores_prior_transcript() -> Result<()> {
    let case_name = "code-resume-happy-path";
    let user_message = "ping-resume-happy-path";

    let repo_root = tempfile::Builder::new()
        .prefix(&format!("{case_name}-"))
        .tempdir()
        .context("failed to create resume tempdir")?;
    let repo_dir = repo_root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).context("failed to create repo subdir")?;
    run_libra_init(&repo_dir)?;

    // First spawn — submit one chat turn and capture the session id.
    let session_id = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture_path())
                .with_existing_repo_dir(repo_dir.clone()),
        )
        .context("first spawn (capture session id)")?;
        // Acquire a controller token so `submit_message` (which goes
        // through the authorized-write path) is not rejected with
        // `MISSING_CONTROLLER_TOKEN`.
        session
            .attach_automation(case_name)
            .context("attach automation before submit")?;
        let status = session
            .submit_message(user_message)
            .context("submit chat turn before shutdown")?;
        if !status.is_success() {
            bail!("submit returned non-success status {status}");
        }
        // Drain the snapshot AFTER submit so we observe the
        // session_id the runtime persisted alongside the user
        // message (the runtime always emits a stable id from
        // first /session GET, but reading post-submit is harmless
        // and lets us confirm the message landed in the in-memory
        // transcript before we shut down).
        let snapshot = session.snapshot().context("snapshot post-submit")?;
        let id = snapshot
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("snapshot did not surface a sessionId field; got {snapshot:?}")
            })?;
        // CodeSession::Drop will issue the clean shutdown that
        // triggers the App::leave save fallback. Allow any
        // outstanding writes to flush by dropping explicitly here.
        drop(session);
        id
    };

    if session_id.trim().is_empty() {
        bail!("captured session_id was empty; cannot resume");
    }

    // Second spawn — resume with the captured chat-flow id.
    let resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture_path())
            .with_existing_repo_dir(repo_dir.clone())
            .with_resume_thread(&session_id),
    )
    .with_context(|| format!("resume spawn for session_id '{session_id}'"))?;

    let snapshot = resumed.snapshot().context("snapshot post-resume")?;
    assert!(
        transcript_contains(&snapshot, user_message),
        "resumed transcript did not include the original user message '{user_message}';\nsnapshot: {snapshot}"
    );
    Ok(())
}

/// Wave 9 §5.16 — SIGTERM-mid-turn resume.
///
/// The Code UI saves the session immediately after accepting a user
/// message, before the provider finishes the assistant response.
/// This test holds the fake provider in a delayed response, sends
/// SIGTERM without `/quit`, then re-spawns with `--resume` and
/// proves the latest submitted user turn is still present.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn resume_after_sigterm_mid_turn_restores_latest_submitted_message() -> Result<()> {
    let case_name = "code-resume-sigterm-mid-turn";
    let user_message = "slow-resume-mid-turn";

    let repo_root = tempfile::Builder::new()
        .prefix(&format!("{case_name}-"))
        .tempdir()
        .context("failed to create resume tempdir")?;
    let repo_dir = repo_root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).context("failed to create repo subdir")?;
    run_libra_init(&repo_dir)?;

    let session_id = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), delayed_fixture_path())
                .with_existing_repo_dir(repo_dir.clone()),
        )
        .context("first spawn (SIGTERM mid-turn)")?;
        session
            .attach_automation(case_name)
            .context("attach automation before delayed submit")?;
        let status = session
            .submit_message(&format!("/chat {user_message}"))
            .context("submit delayed chat turn")?;
        if !status.is_success() {
            bail!("submit returned non-success status {status}");
        }

        let snapshot = session
            .wait_for_snapshot(Duration::from_secs(10), |snapshot| {
                snapshot_status(snapshot) == Some("thinking")
                    && transcript_contains(snapshot, user_message)
            })
            .context("wait for delayed turn to enter thinking state")?;
        let id = snapshot
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("snapshot did not surface a sessionId field; got {snapshot:?}")
            })?;
        session
            .terminate_without_cleanup()
            .context("SIGTERM libra code mid-turn")?;
        id
    };

    let resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), delayed_fixture_path())
            .with_existing_repo_dir(repo_dir.clone())
            .with_resume_thread(&session_id),
    )
    .with_context(|| format!("resume spawn for session_id '{session_id}'"))?;

    let snapshot = resumed
        .wait_for_snapshot(Duration::from_secs(10), |snapshot| {
            transcript_contains(snapshot, user_message)
        })
        .context("wait for resumed transcript to include mid-turn user message")?;
    assert!(
        transcript_contains(&snapshot, user_message),
        "resumed transcript did not include the SIGTERM-mid-turn user message '{user_message}';\nsnapshot: {snapshot}"
    );
    Ok(())
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn resume_test_requires_test_provider_feature() {
    eprintln!("skipping resume test; enable --features test-provider");
}
