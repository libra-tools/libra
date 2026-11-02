//! Local contract tests for `libra mega2 traces`. The server is loopback HTTP.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use super::run_libra_command;

fn serve(
    handler: impl Fn(String, usize) -> (u16, String) + Send + Sync + 'static,
) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    thread::spawn(move || {
        for _ in 0..12 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buffer = [0_u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let text = String::from_utf8_lossy(&buffer[..read]);
            let path = text
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let seen = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let (status, body) = handler(path, seen);
            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, hits)
}

fn capabilities_body(ready: bool) -> String {
    format!(
        r#"{{"protocol":"libra-traces/v1","server_id":"11111111-1111-4111-8111-111111111111","search_ready":{ready},"search_unavailable_reason":"extension_missing"}}"#
    )
}

#[test]
fn capabilities_frame_contract() {
    let (port, _) = serve(|path, _| {
        if path.contains("/capabilities") {
            (200, capabilities_body(true))
        } else {
            (404, r#"{"code":"unknown_scope"}"#.to_string())
        }
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let json = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "capabilities",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(
        text.contains("\"ok\":true") || text.contains("\"ok\": true"),
        "{text}"
    );
    assert!(text.contains("libra-traces/v1"), "{text}");

    let machine = run_libra_command(
        &[
            "--machine",
            "mega2",
            "traces",
            "capabilities",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(
        machine.status.success(),
        "{}",
        String::from_utf8_lossy(&machine.stderr)
    );
    let machine_text = String::from_utf8_lossy(&machine.stdout);
    assert!(
        machine_text.contains("\"operation\":\"capabilities\""),
        "{machine_text}"
    );
    assert!(
        !machine_text.contains("\"ok\""),
        "machine output is the value, not the envelope: {machine_text}"
    );

    let (closed, _) = serve(|_, _| (200, capabilities_body(false)));
    let denied = run_libra_command(
        &[
            "mega2",
            "traces",
            "capabilities",
            "--server",
            &format!("http://127.0.0.1:{closed}"),
            "--token",
            "test-token",
            "--require-search",
        ],
        root.path(),
    );
    assert!(!denied.status.success());
    let stderr = String::from_utf8_lossy(&denied.stderr);
    assert!(stderr.contains("search is not ready"), "{stderr}");
    assert!(stderr.contains("Error-Code:"), "{stderr}");
}

#[test]
fn search_readiness_preflight_contract() {
    let (port, hits) = serve(|path, seen| {
        assert!(path.contains("/capabilities"), "{path}");
        assert!(!path.contains("/search"), "{path}");
        let body = match seen {
            1 => capabilities_body(false),
            2 => {
                r#"{"protocol":"libra-traces/v1","server_id":"11111111-1111-4111-8111-111111111111"}"#
                    .to_string()
            }
            _ => {
                r#"{"protocol":"libra-traces/v1","server_id":"11111111-1111-4111-8111-111111111111","search_ready":"yes"}"#
                    .to_string()
            }
        };
        (200, body)
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let output = run_libra_command(
        &[
            "mega2",
            "traces",
            "capabilities",
            "--require-search",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("extension_missing"));
    let missing = run_libra_command(
        &[
            "mega2",
            "traces",
            "capabilities",
            "--require-search",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing"));
    let untyped = run_libra_command(
        &[
            "mega2",
            "traces",
            "capabilities",
            "--require-search",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(!untyped.status.success());
    assert!(String::from_utf8_lossy(&untyped.stderr).contains("boolean"));
    let plain = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "capabilities",
            "--server",
            &server,
            "--token",
            "test-token",
        ],
        root.path(),
    );
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(String::from_utf8_lossy(&plain.stdout).contains("libra-traces/v1"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 4);
}

#[test]
fn target_resolve_navigation_contract() {
    let (port, _) = serve(|path, _| {
        assert!(
            path.contains("path=%2Fproject%2Ffoo") || path.contains("path=/project/foo"),
            "{path}"
        );
        (
            200,
            r#"{"target_id":"target-1","path":"/project/foo"}"#.to_string(),
        )
    });
    let root = tempfile::tempdir().expect("root");
    let output = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "targets",
            "--server",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
            "--path",
            "/project/foo",
        ],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("target-1"));
}

#[test]
fn target_stream_navigation_contract() {
    let (port, hits) = serve(|path, _| {
        assert!(path.contains("source=opaque-legacy-value"), "{path}");
        assert!(path.contains("owner=owner-1"), "{path}");
        assert!(path.contains("epoch=epoch-1"), "{path}");
        assert!(path.contains("cursor=page-1"), "{path}");
        assert!(path.contains("limit=50"), "{path}");
        (
            200,
            r#"{"items":[{"stream_id":"stream-1"}],"next_cursor":null}"#.to_string(),
        )
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let output = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "streams",
            "--server",
            &server,
            "--token",
            "test-token",
            "--target",
            "target-1",
            "--source",
            "opaque-legacy-value",
            "--owner",
            "owner-1",
            "--epoch",
            "epoch-1",
            "--cursor",
            "page-1",
        ],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("stream-1"));
    let too_many = run_libra_command(
        &[
            "mega2",
            "traces",
            "streams",
            "--server",
            &server,
            "--token",
            "test-token",
            "--target",
            "target-1",
            "--limit",
            "201",
        ],
        root.path(),
    );
    assert!(!too_many.status.success());
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn session_navigation_contract() {
    let (port, _) = serve(|path, seen| {
        if path.contains("/sessions/session-1") {
            (200, r#"{"session_id":"session-1","accepted_watermark":"keep","code_link":"unresolved"}"#.to_string())
        } else {
            assert_eq!(seen, 1, "{path}");
            assert!(path.contains("agent=claude_code"), "{path}");
            assert!(path.contains("session=session-keep"), "{path}");
            assert!(path.contains("cursor=page-2"), "{path}");
            assert!(
                path.contains("code_commit=0123456789abcdef0123456789abcdef01234567"),
                "{path}"
            );
            (200, r#"{"items":[{"session_id":"session-1"}]}"#.to_string())
        }
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let list = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "sessions",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--agent",
            "claude_code",
            "--session",
            "session-keep",
            "--cursor",
            "page-2",
            "--code-commit",
            "0123456789abcdef0123456789abcdef01234567",
        ],
        root.path(),
    );
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let show = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "show",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--session",
            "session-1",
        ],
        root.path(),
    );
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    assert!(show_text.contains("session-1"), "{show_text}");
    assert!(show_text.contains("unresolved"), "{show_text}");
    let both = run_libra_command(
        &[
            "mega2",
            "traces",
            "show",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--session",
            "session-1",
            "--checkpoint",
            "cp-1",
        ],
        root.path(),
    );
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("not both"));
}

#[test]
fn session_time_filter_contract() {
    let (port, hits) = serve(|path, _| {
        assert!(
            path.contains("since=2026-11-02T00%3A00%3A00Z")
                || path.contains("since=2026-11-02T00:00:00Z"),
            "{path}"
        );
        (200, r#"{"items":[]}"#.to_string())
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let output = run_libra_command(
        &[
            "mega2",
            "traces",
            "sessions",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--since",
            "2026-11-02T08:00:00+08:00",
            "--until",
            "2026-11-02T09:00:00+08:00",
        ],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let empty = run_libra_command(
        &[
            "mega2",
            "traces",
            "sessions",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--since",
            "2026-11-02T00:00:00Z",
            "--until",
            "2026-11-02T00:00:00Z",
        ],
        root.path(),
    );
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let rejected = run_libra_command(
        &[
            "mega2",
            "traces",
            "sessions",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--since",
            "2026-11-02T02:00:00Z",
            "--until",
            "2026-11-02T01:00:00Z",
        ],
        root.path(),
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("since must be earlier"));
    let bad_oid = run_libra_command(
        &[
            "mega2",
            "traces",
            "sessions",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--code-commit",
            "abcd",
        ],
        root.path(),
    );
    assert!(!bad_oid.status.success());
    assert!(String::from_utf8_lossy(&bad_oid.stderr).contains("hexadecimal"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn checkpoint_navigation_contract() {
    let (port, _) = serve(|path, _| {
        if path.contains("/checkpoints/cp-1") && !path.contains("transcript") {
            (
                200,
                r#"{"checkpoint_id":"cp-1","accepted_watermark":"a","indexed_watermark":"b","unsupported":1,"invalid":2,"code_link":"unresolved"}"#.to_string(),
            )
        } else {
            assert!(path.contains("cursor=page-3"), "{path}");
            (200, r#"{"items":[{"checkpoint_id":"cp-1"}]}"#.to_string())
        }
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let list = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "checkpoints",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--session",
            "session-1",
            "--cursor",
            "page-3",
        ],
        root.path(),
    );
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let show = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "show",
            "--server",
            &server,
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--checkpoint",
            "cp-1",
        ],
        root.path(),
    );
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    assert!(show_text.contains("cp-1"), "{show_text}");
    assert!(show_text.contains("unresolved"), "{show_text}");
    assert!(
        show_text.contains("\"unsupported\":1") || show_text.contains("\"unsupported\": 1"),
        "{show_text}"
    );
}

#[test]
fn keyword_search_contract_matrix() {
    let (port, _) = serve(|path, _| {
        if path.contains("/search") {
            assert!(path.contains("limit=20"), "{path}");
            assert!(path.contains("max_bytes=16384"), "{path}");
            if path.contains("%E4%BD%A0") || path.contains("q=%E4%BD%A0") || path.contains("q=你")
            {
                (
                    200,
                    r#"{"items":[],"coverage_complete":false,"index_state":"pending"}"#.to_string(),
                )
            } else {
                (
                    503,
                    r#"{"code":"search_unavailable","reason":"extension_missing"}"#.to_string(),
                )
            }
        } else {
            (200, r#"{"target_id":"target-1"}"#.to_string())
        }
    });
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    let found = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "search",
            "--server",
            &server,
            "--token",
            "test-token",
            "--target",
            "/project/foo",
            "--query",
            "你",
        ],
        root.path(),
    );
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
    assert!(String::from_utf8_lossy(&found.stdout).contains("pending"));
    let unavailable = run_libra_command(
        &[
            "mega2",
            "traces",
            "search",
            "--server",
            &server,
            "--token",
            "test-token",
            "--target",
            "/project/foo",
            "--query",
            "timeout",
        ],
        root.path(),
    );
    assert!(!unavailable.status.success());
    let stderr = String::from_utf8_lossy(&unavailable.stderr);
    assert!(stderr.contains("search_unavailable"), "{stderr}");
}

#[test]
fn search_local_admission_contract() {
    let (port, hits) = serve(|_, _| (200, r#"{"target_id":"target-1"}"#.to_string()));
    let root = tempfile::tempdir().expect("root");
    let server = format!("http://127.0.0.1:{port}");
    for query in ["", &"a".repeat(257)] {
        let output = run_libra_command(
            &[
                "mega2",
                "traces",
                "search",
                "--server",
                &server,
                "--token",
                "test-token",
                "--target",
                "/project/foo",
                "--query",
                query,
            ],
            root.path(),
        );
        assert!(!output.status.success(), "{query}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("was not sent"));
    }
    thread::sleep(Duration::from_millis(50));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    let allowed = run_libra_command(
        &[
            "mega2",
            "traces",
            "search",
            "--server",
            &server,
            "--token",
            "test-token",
            "--target",
            "/project/foo",
            "--query",
            "a",
        ],
        root.path(),
    );
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
}

#[test]
fn bounded_evidence_read_matrix() {
    let (port, _) = serve(|path, _| {
        assert!(path.contains("max_bytes=16384"), "{path}");
        (
            200,
            r#"{"text":"hello","partial":false,"next_cursor":null}"#.to_string(),
        )
    });
    let root = tempfile::tempdir().expect("root");
    let output = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "transcript",
            "--server",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
            "--stream",
            "stream-1",
            "--checkpoint",
            "cp-1",
        ],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
}

fn git_blob_sha1(payload: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", payload.len()).as_bytes());
    hasher.update(payload.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn explicit_fetch_scope_and_local_invariants() {
    let payload = "evidence";
    let id = git_blob_sha1(payload);
    let body = format!(
        r#"{{"hash_kind":"sha1","stream":"44444444-4444-4444-8444-444444444444","tip":"{id}","objects":[{{"id":"{id}","kind":"blob","payload":"{payload}","links":[]}}]}}"#
    );
    let primary = "11111111-1111-4111-8111-111111111111";
    let other = "22222222-2222-4222-8222-222222222222";
    let (port, hits) = serve(move |path, seen| {
        if path.contains("/capabilities") {
            let server_id = if seen > 2 { other } else { primary };
            (
                200,
                format!(
                    r#"{{"protocol":"libra-traces/v1","server_id":"{server_id}","search_ready":true}}"#
                ),
            )
        } else {
            assert!(path.contains("/archive"), "{path}");
            assert!(
                seen <= 2,
                "a fingerprint conflict must not download the archive"
            );
            (200, body.clone())
        }
    });
    let root = tempfile::tempdir().expect("root");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run_libra_command(&["init"], &repo).status.success());
    std::fs::write(repo.join("keep.txt"), "same").unwrap();
    let remote = format!("http://127.0.0.1:{port}/project/foo.git");
    assert!(
        run_libra_command(&["config", "set", "remote.origin.url", &remote], &repo)
            .status
            .success()
    );
    let stream = "44444444-4444-4444-8444-444444444444";
    let anonymous = run_libra_command(
        &[
            "mega2", "traces", "fetch", "--remote", "origin", "--stream", stream,
        ],
        &repo,
    );
    assert!(!anonymous.status.success());
    assert!(String::from_utf8_lossy(&anonymous.stderr).contains("anonymous"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    let output = run_libra_command(
        &[
            "--json",
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            stream,
            "--token",
            "test-token",
        ],
        &repo,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("refs/remotes/origin/traces/"), "{text}");
    assert!(
        text.contains("\"hash_kind\":\"sha1\"") || text.contains("\"hash_kind\": \"sha1\""),
        "{text}"
    );
    assert!(!text.contains("refs/heads/traces\""));
    let stored = run_libra_command(
        &[
            "config",
            "get",
            &format!("libra.traces.archive.{stream}.ref"),
        ],
        &repo,
    );
    assert!(
        stored.status.success(),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );
    let stored_text = String::from_utf8_lossy(&stored.stdout);
    assert!(
        stored_text.contains("refs/remotes/origin/traces/"),
        "{stored_text}"
    );
    assert!(!stored_text.contains("refs/libra/traces"));
    let fingerprint = run_libra_command(
        &["config", "get", "libra.traces.remote.origin.fingerprint"],
        &repo,
    );
    assert!(
        fingerprint.status.success(),
        "{}",
        String::from_utf8_lossy(&fingerprint.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("keep.txt")).unwrap(),
        "same"
    );
    let conflict = run_libra_command(
        &[
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            stream,
            "--token",
            "test-token",
        ],
        &repo,
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("does not match"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    let _ = Path::new(&repo);
}
