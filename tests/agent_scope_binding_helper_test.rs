//! Built-binary regression for the private Session Capture scope-binding
//! helper. The helper is intentionally callable by another same-user process,
//! so its wire output must remain safe without relying on a caller capability.

#![cfg(unix)]

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libra::internal::ai::hooks::runtime::{
    CAPTURE_SCOPE_BINDING_HELPER_ARG, CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP,
};
use serde_json::json;

fn run_libra(repo: &Path, home: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("LIBRA_TEST_HOME", home)
        .env("LIBRA_COMMITTER_NAME", "Scope Helper Test")
        .env("LIBRA_COMMITTER_EMAIL", "scope-helper@test.libra")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn libra binary");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("helper stdin is piped")
            .write_all(input)
            .expect("write helper frame");
    }
    child.wait_with_output().expect("wait for libra binary")
}

#[test]
fn built_scope_binding_helper_hides_key_and_fails_closed_on_bad_frames() {
    let root = tempfile::tempdir().expect("create isolated helper test root");
    let home = root.path().join("home");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&home).expect("create fake home");
    std::fs::create_dir_all(&repo).expect("create repository directory");
    let repo = repo
        .canonicalize()
        .expect("canonicalize repository directory");

    let init = run_libra(&repo, &home, &["init"], None);
    assert!(
        init.status.success(),
        "initialize helper repository: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let deadline_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock is after Unix epoch")
        .as_millis()
        .checked_add(30_000)
        .and_then(|millis| i64::try_from(millis).ok())
        .expect("helper deadline fits i64");
    let frame = serde_json::to_vec(&json!({
        "reported_cwd": repo.to_string_lossy(),
        "deadline_millis": deadline_millis,
        "event_identity_preimage": vec![0xC4_u8; 32],
        "dedup_preimage": vec![0xD4_u8; 32],
    }))
    .expect("serialize valid helper request");

    let first = run_libra(
        &repo,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&frame),
    );
    assert!(
        first.status.success(),
        "valid helper request failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        first.stdout.first(),
        Some(&b'T'),
        "a valid active worktree must emit the trusted proof phase"
    );
    let key = std::fs::read(
        repo.join(".libra")
            .join("private")
            .join("agent-capture-dedup-v1.key"),
    )
    .expect("helper creates its repository-private replay key");
    let rendered = String::from_utf8_lossy(&first.stdout);
    assert!(
        !rendered.contains("dedup_secret")
            && !rendered.contains("secret_base64")
            && !rendered.contains(&STANDARD.encode(&key))
            && !rendered.contains(&hex::encode(&key)),
        "the built helper must never emit repository key material: {rendered}"
    );
    let response: serde_json::Value =
        serde_json::from_slice(&first.stdout[1..]).expect("trusted helper response is JSON");
    let opaque = response
        .get("opaque_dedup")
        .and_then(serde_json::Value::as_object)
        .expect("valid helper response contains only an opaque replay identity");
    assert_eq!(opaque.len(), 2, "opaque identity has a closed wire shape");
    assert!(
        opaque.contains_key("event_id") && opaque.contains_key("dedup_key"),
        "opaque identity is limited to its event and receipt identities"
    );
    let opaque_event = response
        .get("opaque_event")
        .and_then(serde_json::Value::as_object)
        .expect("valid helper response contains an opaque action identity");
    assert_eq!(
        opaque_event.len(),
        2,
        "opaque action identity has a closed wire shape"
    );
    assert!(
        opaque_event.contains_key("event_id") && opaque_event.contains_key("event_key"),
        "opaque action identity is limited to its UUID and short-lived HMAC proof"
    );
    assert_eq!(
        opaque_event.get("event_id"),
        opaque.get("event_id"),
        "native v2 event-id behavior must remain byte-for-byte unchanged"
    );
    assert_eq!(
        opaque_event.get("event_key"),
        opaque.get("dedup_key"),
        "native v2 receipt HMAC remains the native action-identity proof"
    );

    let replay = run_libra(
        &repo,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&frame),
    );
    assert!(replay.status.success(), "duplicate helper request succeeds");
    assert_eq!(
        first.stdout, replay.stdout,
        "the same fixed ingress commitment must replay to one opaque identity"
    );

    let raw_session_sentinel = "raw-session-must-not-enter-helper-wire";
    let fallback_frame = serde_json::to_vec(&json!({
        "reported_cwd": repo.to_string_lossy(),
        "deadline_millis": deadline_millis,
        "event_identity_preimage": vec![0xA7_u8; 32],
        "dedup_preimage": serde_json::Value::Null,
    }))
    .expect("serialize no-native helper request");
    let fallback = run_libra(
        &repo,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&fallback_frame),
    );
    assert!(
        fallback.status.success() && fallback.stdout.first() == Some(&b'T'),
        "a no-native helper request must still produce an opaque action identity: {}",
        String::from_utf8_lossy(&fallback.stderr)
    );
    let fallback_rendered = String::from_utf8_lossy(&fallback.stdout);
    assert!(
        !fallback_rendered.contains(raw_session_sentinel)
            && !fallback_rendered.contains(&STANDARD.encode(&key))
            && !fallback_rendered.contains(&hex::encode(&key)),
        "no-native helper output must not expose raw session input or repository key material: {fallback_rendered}"
    );
    let fallback_response: serde_json::Value = serde_json::from_slice(&fallback.stdout[1..])
        .expect("no-native helper response is JSON after its trusted phase");
    assert!(
        fallback_response
            .get("opaque_dedup")
            .is_some_and(serde_json::Value::is_null),
        "no native identity must remain receipt-free"
    );
    let fallback_event = fallback_response
        .get("opaque_event")
        .and_then(serde_json::Value::as_object)
        .expect("no-native response carries only an opaque action identity");
    assert_eq!(fallback_event.len(), 2);
    assert!(
        fallback_event
            .get("event_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.starts_with("capture-event-v1:")),
        "no-native action identity must use the distinct opaque event HMAC domain"
    );

    let malformed = run_libra(
        &repo,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(b"{not-json"),
    );
    assert!(
        malformed.status.success() && malformed.stdout == b"U",
        "a malformed request must be rejected at the unverified phase without a response: status={:?}, stdout={:?}, stderr={}",
        malformed.status.code(),
        malformed.stdout,
        String::from_utf8_lossy(&malformed.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&malformed.stdout).contains(&STANDARD.encode(&key)),
        "a malformed request must not emit key material"
    );

    let oversized = vec![b'x'; CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP as usize + 1];
    let oversized = run_libra(
        &repo,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&oversized),
    );
    assert_eq!(
        oversized.status.code(),
        Some(2),
        "the binary entrypoint must reject an oversized helper frame before decoding"
    );
    assert!(
        oversized.stdout.is_empty(),
        "an oversized helper request must emit no phase or response"
    );
}

/// R86 #4: a helper invoked from a directory outside every Libra repository
/// has no active scope. It must emit only the advisory `N` proof (never the
/// trusted `T` phase or a trusted-failure response) and must not create any
/// repository or replay-key state.
#[test]
fn built_scope_binding_helper_reports_no_repository_outside_any_repository() {
    let root = tempfile::tempdir().expect("create isolated helper test root");
    let home = root.path().join("home");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&home).expect("create fake home");
    std::fs::create_dir_all(&outside).expect("create outside-repository directory");
    let outside = outside
        .canonicalize()
        .expect("canonicalize outside-repository directory");

    let deadline_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock is after Unix epoch")
        .as_millis()
        .checked_add(30_000)
        .and_then(|millis| i64::try_from(millis).ok())
        .expect("helper deadline fits i64");
    let frame = serde_json::to_vec(&json!({
        "reported_cwd": outside.to_string_lossy(),
        "deadline_millis": deadline_millis,
        "event_identity_preimage": vec![0xB3_u8; 32],
        "dedup_preimage": vec![0xE5_u8; 32],
    }))
    .expect("serialize valid outside-repository helper request");

    let out = run_libra(
        &outside,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&frame),
    );
    assert!(
        out.status.success(),
        "an outside-repository helper request must complete its proof protocol: status={:?}, stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"N",
        "outside every repository the helper must emit only the advisory no-repository proof, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !outside.join(".libra").exists(),
        "an outside-repository helper request must not create repository or replay-key state"
    );
}

/// R88 #8: a helper invoked from a linked worktree whose `commondir` is
/// corrupt is inside an active repository that cannot be resolved. It must
/// emit the trusted `T` proof followed only by the closed, content-free
/// `storage_unresolved` repository class — never the discovery error (which
/// names local paths) and never any replay-key state — so the parent can
/// restore the shipped `LBR-REPO-003` contract.
#[test]
fn built_scope_binding_helper_classifies_a_damaged_active_worktree() {
    let root = tempfile::tempdir().expect("create isolated helper test root");
    let home = root.path().join("home");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&home).expect("create fake home");
    std::fs::create_dir_all(&repo).expect("create repository directory");
    let repo = repo
        .canonicalize()
        .expect("canonicalize repository directory");
    let init = run_libra(&repo, &home, &["init"], None);
    assert!(
        init.status.success(),
        "initialize helper repository: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let linked = root.path().join("linked");
    let linked_arg = linked.to_string_lossy().into_owned();
    let added = run_libra(&repo, &home, &["worktree", "add", &linked_arg], None);
    assert!(
        added.status.success(),
        "create linked worktree: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let linked = linked.canonicalize().expect("canonicalize linked worktree");
    std::fs::write(
        linked.join(".libra").join("commondir"),
        "../missing-helper-storage\n",
    )
    .expect("break linked-worktree commondir");

    let deadline_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock is after Unix epoch")
        .as_millis()
        .checked_add(30_000)
        .and_then(|millis| i64::try_from(millis).ok())
        .expect("helper deadline fits i64");
    let frame = serde_json::to_vec(&json!({
        "reported_cwd": linked.to_string_lossy(),
        "deadline_millis": deadline_millis,
        "event_identity_preimage": vec![0x5A_u8; 32],
        "dedup_preimage": vec![0x6B_u8; 32],
    }))
    .expect("serialize damaged-worktree helper request");

    let out = run_libra(
        &linked,
        &home,
        &[CAPTURE_SCOPE_BINDING_HELPER_ARG],
        Some(&frame),
    );
    assert!(
        out.status.success(),
        "a damaged-worktree helper request must complete its proof protocol: status={:?}, stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout.first(),
        Some(&b'T'),
        "a damaged active worktree is trusted infrastructure: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&out.stdout[1..]).expect("trusted helper response is JSON");
    assert_eq!(
        response,
        json!({"result": "trusted_repository_failure", "reason": "storage_unresolved"}),
        "only the closed repository class may cross the helper wire"
    );
    let rendered = String::from_utf8_lossy(&out.stdout);
    for path in [&linked, &repo, root.path()] {
        let spelled = path.to_string_lossy();
        assert!(
            !rendered.contains(&*spelled)
                && !spelled
                    .strip_prefix("/private")
                    .is_some_and(|alias| rendered.contains(alias)),
            "the classified helper response must stay path-free: {rendered}"
        );
    }
    assert!(
        !rendered.contains("missing-helper-storage"),
        "the discovery error must not cross the helper wire: {rendered}"
    );
    assert!(
        !linked.join(".libra").join("private").exists(),
        "a damaged worktree must not create replay-key state"
    );
}
