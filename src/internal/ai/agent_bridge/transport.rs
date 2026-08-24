//! Bridge NDJSON transport (plan-20260818 LB-01).
//!
//! Reads newline-delimited JSON-RPC 2.0 frames from stdin and writes exactly
//! one NDJSON frame per response/notification to stdout. Diagnostics never
//! reach stdout (GC-LB-04): the caller is responsible for routing all logs to
//! stderr. Enforces the v1 frame cap and the in-flight request bound, and
//! guarantees reader/writer/timer guards are released on EOF/error/cancel
//! (GC-LB-10).

use std::{
    io::{self, BufRead, Write},
    panic::AssertUnwindSafe,
    time::Duration,
};

use async_trait::async_trait;
use futures::FutureExt;
use serde_json::Value;
use tokio::time::timeout;

use super::protocol::{BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION, MAX_INFLIGHT};

// `?Send`: a handler may reach the real VCS services, whose internals use
// thread-local `Rc`/`RefCell` caches across awaits (see `command::diff`). The
// bridge drives exactly one request future at a time on the stdio loop's own
// task and never spawns it, so a `Send` future buys nothing here — while
// requiring it would force those services to be rewritten purely to satisfy a
// bound the transport does not use. The handler VALUE stays `Send + Sync`.
#[async_trait(?Send)]
pub trait BridgeHandler: Send + Sync {
    /// Handle one validated request and produce a response (or `None` for a
    /// notification that produces no reply). Failures should be returned as a
    /// `BridgeResponse` error rather than a panic; panics are caught by the
    /// transport and turned into a stable internal error so one bad request
    /// cannot tear down the connection.
    async fn handle(&self, request: &BridgeRequest) -> Result<Option<BridgeResponse>, BridgeError>;
}

/// A minimal handler implementing only the v1 handshake. Used by LB-01; the
/// full dispatcher (LB-03..LB-06) replaces it.
pub struct HandshakeHandler;

#[async_trait(?Send)]
impl BridgeHandler for HandshakeHandler {
    async fn handle(&self, request: &BridgeRequest) -> Result<Option<BridgeResponse>, BridgeError> {
        crate::internal::ai::agent_bridge::protocol::dispatch_handshake(request)
    }
}

/// Run the bridge loop over the given reader/writer. `reader` is normally
/// stdin and `writer` stdout, but tests inject in-memory buffers.
///
/// `request_deadline` is the per-request timeout applied to every dispatched
/// method; the CLI passes the v1 default (`REQUEST_DEADLINE_SECS`), tests pass
/// a short one so the timeout path is exercised quickly.
///
/// Returns the count of frames read, so tests can assert how much was
/// consumed. The loop terminates cleanly on EOF.
pub async fn run<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    handler: &dyn BridgeHandler,
    request_deadline: Duration,
) -> io::Result<u64> {
    let mut in_flight = 0usize;
    let mut frames = 0u64;
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        // Read one bounded NDJSON frame. `read_bounded_line` stops buffering
        // once the line exceeds MAX_FRAME_BYTES (even without a trailing
        // newline), so an adversarial/unterminated write cannot grow memory
        // past the v1 cap (GC-LB-11). It drains the remainder of an oversized
        // line to stay in sync with the stream and reports that it was
        // oversized so the caller can emit a stable frame-too-large error.
        let (line, oversized) = match read_bounded_line(&mut reader)? {
            ReadOutcome::Eof => break,
            ReadOutcome::Line(line, oversized) => (line, oversized),
        };
        frames += 1;

        let trimmed = std::str::from_utf8(&line).unwrap_or("");
        let trimmed = trimmed.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Intentionally lenient: a blank line is skipped, not error'd.
            continue;
        }

        // Frame byte cap first (GC-LB-11). read_bounded_line already bounded
        // buffering; `oversized` also catches a line that exceeded the cap and
        // was drained without a trailing newline.
        if oversized || trimmed.len() > super::protocol::MAX_FRAME_BYTES {
            // We cannot parse the id from an oversized frame safely; respond
            // with a null id.
            write_response(
                &mut writer,
                BridgeError::frame_too_large(trimmed.len()).into_object(Value::Null),
            )?;
            continue;
        }

        // Parse the frame. JSON-RPC 2.0 requires a parse error to be answered
        // with a null id (the frame is unparseable, so its id cannot be
        // trusted/extracted).
        let request = match super::protocol::parse_request_line(trimmed) {
            Ok(request) => request,
            Err(err) => {
                write_response(&mut writer, err.into_object(Value::Null))?;
                continue;
            }
        };

        // A JSON-RPC 2.0 notification (no id) never receives a reply — not
        // even an error frame. Dispatch it so its side effects still happen,
        // but suppress the response.
        let is_notification = request.id.is_none();
        let id = request.id.clone().unwrap_or(Value::Null);

        // Duplicate request-id detection (GC-LB-05): a repeated non-null id
        // is rejected fail-closed rather than silently answered a second time.
        if !is_notification && !seen_ids.insert(id.to_string()) {
            let err = BridgeError::invalid_request(format!(
                "duplicate request id '{}' on this connection; refusing (GC-LB-05)",
                id
            ));
            write_response(&mut writer, err.into_object(id))?;
            continue;
        }

        // Validate the method against the allowlist before dispatching.
        if let Err(err) = super::protocol::validate_method(&request.method) {
            if !is_notification {
                write_response(&mut writer, err.into_object(id))?;
            }
            continue;
        }

        // In-flight bound (GC-LB-11). The loop is fully sequential — each
        // request is awaited to completion before the next line is read — so
        // `in_flight` never exceeds 1 in practice. The bound is kept as a
        // defensive cap so a future concurrent dispatch cannot silently exceed
        // the advertised v1 limit (MAX_INFLIGHT = 64).
        if in_flight >= MAX_INFLIGHT {
            let err = BridgeError::inflight_limit();
            if !is_notification {
                write_response(&mut writer, err.into_object(id))?;
            }
            continue;
        }
        in_flight += 1;

        // Dispatch with a hard request deadline (GC-LB-11). A handler that
        // stalls past the v1 default deadline (30 s) is failed with a stable,
        // retryable timeout error rather than blocking the loop forever.
        //
        // A panic inside a handler is caught (AssertUnwindSafe + catch_unwind)
        // and converted into a stable internal error so one bad request cannot
        // tear down the whole connection — matching the contract documented on
        // `BridgeHandler::handle`.
        let outcome = timeout(
            request_deadline,
            AssertUnwindSafe(handler.handle(&request)).catch_unwind(),
        )
        .await;

        let response = match outcome {
            Err(_elapsed) => BridgeError::timeout(format!(
                "bridge method '{}' exceeded the {}s request deadline",
                request.method,
                request_deadline.as_secs()
            ))
            .into_object(id),
            Ok(Err(panic)) => {
                // A handler panicked. Report it on stderr (never stdout,
                // GC-LB-04) and answer the peer with a stable internal error.
                let msg = panic_message(&panic);
                eprintln!(
                    "libra agent bridge: request '{}' panicked ({msg}); answered fail-closed",
                    request.method
                );
                BridgeError::internal(format!(
                    "bridge method '{}' panicked while handling the request",
                    request.method
                ))
                .into_object(id)
            }
            Ok(Ok(Ok(Some(response)))) => response,
            Ok(Ok(Ok(None))) => {
                // Notification with no reply — nothing to write.
                in_flight -= 1;
                continue;
            }
            Ok(Ok(Err(err))) => err.into_object(id),
        };
        in_flight -= 1;

        // Notifications get no reply (JSON-RPC 2.0).
        if !is_notification {
            write_response(&mut writer, response)?;
        }
    }

    let _ = writer.flush();
    Ok(frames)
}

/// What a bounded frame read produced.
enum ReadOutcome {
    /// Clean EOF before any byte of a new frame.
    Eof,
    /// One NDJSON line (bounded to `MAX_FRAME_BYTES`), with or without a
    /// trailing newline. The bool is `true` when the underlying line exceeded
    /// the cap and was drained (so the caller reports frame-too-large even
    /// without a trailing newline).
    Line(Vec<u8>, bool),
}

/// Read exactly one NDJSON line, stopping buffering once it exceeds
/// `MAX_FRAME_BYTES` (GC-LB-11). A line with no trailing newline is still
/// returned on EOF. Once the cap is exceeded the rest of the line is drained
/// (not buffered) so the stream stays in sync, and the returned flag tells the
/// caller the line was oversized. `R: BufRead` keeps this efficient:
/// `bytes()` reads from the already-buffered data.
fn read_bounded_line<R: BufRead>(reader: &mut R) -> io::Result<ReadOutcome> {
    use std::io::Read;
    let mut line: Vec<u8> = Vec::new();
    let mut oversized = false;
    let mut saw_byte = false;
    for byte in reader.bytes() {
        let byte = byte?;
        if byte == b'\n' {
            return Ok(ReadOutcome::Line(line, oversized));
        }
        saw_byte = true;
        if line.len() >= super::protocol::MAX_FRAME_BYTES {
            oversized = true;
            continue; // drain, don't buffer
        }
        line.push(byte);
    }
    if !saw_byte {
        Ok(ReadOutcome::Eof)
    } else {
        // Unterminated final line (no trailing newline).
        Ok(ReadOutcome::Line(line, oversized))
    }
}

/// Extract a short, safe description of a caught panic payload. Panic
/// payloads may be strings or arbitrary `Any`; we never print raw data to
/// stdout (GC-LB-04) — only a bounded, diagnostic-only summary to stderr.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "non-string panic payload"
    }
}

/// Encode a response as a single NDJSON frame. A response larger than the
/// result cap (`MAX_RESULT_BYTES`) is refused fail-closed rather than
/// truncated, so stdout never emits an oversized NDJSON line that a
/// line-bounded peer could mis-parse (GC-LB-04 / GC-LB-11).
fn write_response<W: Write>(writer: &mut W, response: BridgeResponse) -> io::Result<()> {
    let json = match serde_json::to_string(&response) {
        Ok(json) => json,
        Err(e) => {
            // Serialization of the error object itself must not fail; if it
            // does we emit a minimal JSON-RPC internal-error frame.
            let fallback = format!(
                r#"{{"jsonrpc":"{JSONRPC_VERSION}","error":{{"code":-32603,"message":"bridge response serialization failed: {e}"}},"id":null}}"#
            );
            writer.write_all(fallback.as_bytes())?;
            writer.write_all(b"\n")?;
            return Ok(());
        }
    };
    if json.len() > super::protocol::MAX_RESULT_BYTES {
        // The result is over the v1 result cap. Refuse it fail-closed rather
        // than emitting an oversized frame. Preserve the request id.
        let id = response.id;
        let err = super::protocol::BridgeError::frame_too_large(json.len()).into_object(id);
        let refused = match serde_json::to_string(&err) {
            Ok(s) => s,
            Err(_) => format!(
                r#"{{"jsonrpc":"{JSONRPC_VERSION}","error":{{"code":-32603,"message":"bridge response too large and could not serialize the refusal"}},"id":null}}"#
            ),
        };
        writer.write_all(refused.as_bytes())?;
        writer.write_all(b"\n")?;
        return Ok(());
    }
    writer.write_all(json.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    fn test_deadline() -> Duration {
        Duration::from_secs(30)
    }

    fn short_deadline() -> Duration {
        Duration::from_millis(200)
    }

    use super::*;

    #[tokio::test]
    async fn eof_terminates_cleanly() {
        let input = "";
        let mut out = Vec::new();
        let n = run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("clean EOF");
        assert_eq!(n, 0);
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn responds_to_initialize() {
        let input = r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}
"#;
        let mut out = Vec::new();
        let n = run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        assert_eq!(n, 1);
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("\"id\":1"), "got: {text}");
        assert!(text.contains("\"result\""));
    }

    #[tokio::test]
    async fn unknown_method_is_rejected_fail_closed() {
        let input = r#"{"jsonrpc":"2.0","method":"create_intent","id":7}
"#;
        let mut out = Vec::new();
        run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("\"code\":-32601"), "got: {text}");
        assert!(text.contains("\"id\":7"));
    }

    #[tokio::test]
    async fn malformed_frame_is_rejected_without_poisoning_stdout() {
        let input = "not-json\n{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"id\":2}\n";
        let mut out = Vec::new();
        run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        // Both lines produce exactly two NDJSON frames, each parseable.
        let frames: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(frames.len(), 2, "got: {text}");
        for frame in frames {
            serde_json::from_str::<serde_json::Value>(frame)
                .expect("every stdout line is a valid JSON frame");
        }
    }

    #[tokio::test]
    async fn loop_survives_many_sequential_requests() {
        // The v1 loop is deliberately sequential (each request is awaited to
        // completion before the next line is read), so `in_flight` never
        // exceeds 1 in practice and the MAX_INFLIGHT guard is a defensive cap
        // for a future concurrent dispatch. What this test can verify for a
        // sequential loop is the real contract: the connection is not torn
        // down by serving far more than MAX_INFLIGHT requests, and every
        // stdout line remains a parseable JSON frame (GC-LB-04).
        let mut body = String::new();
        for _ in 0..MAX_INFLIGHT + 2 {
            body.push_str(r#"{"jsonrpc":"2.0","method":"status.get","id":0}"#);
            body.push('\n');
        }
        let mut out = Vec::new();
        run(
            Cursor::new(body),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        let frames: Vec<&str> = text
            .trim_end()
            .split('\n')
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(frames.len(), MAX_INFLIGHT + 2);
        for frame in frames {
            serde_json::from_str::<serde_json::Value>(frame)
                .expect("every stdout line is a valid JSON frame");
        }
    }

    /// A handler that panics on a specific method, to verify the transport
    /// catches the panic and answers fail-closed instead of tearing the
    /// connection down. Uses an allowlisted method so dispatch reaches the
    /// handler (the transport rejects unknown methods before dispatch).
    struct PanicHandler;

    #[async_trait(?Send)]
    impl BridgeHandler for PanicHandler {
        async fn handle(
            &self,
            request: &BridgeRequest,
        ) -> Result<Option<BridgeResponse>, BridgeError> {
            if request.method == "diff.get" {
                panic!("injected handler panic");
            }
            Ok(Some(BridgeResponse {
                jsonrpc: JSONRPC_VERSION,
                result: Some(serde_json::json!({ "ok": true })),
                error: None,
                id: request.id.clone().unwrap_or(Value::Null),
            }))
        }
    }

    #[tokio::test]
    async fn handler_panic_is_caught_and_answered_fail_closed() {
        let input = r#"{"jsonrpc":"2.0","method":"diff.get","id":9}
{"jsonrpc":"2.0","method":"status.get","id":10}
"#;
        let mut out = Vec::new();
        run(Cursor::new(input), &mut out, &PanicHandler, test_deadline())
            .await
            .expect("runs despite the panic");
        let text = String::from_utf8(out).expect("utf8");
        let frames: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(
            frames.len(),
            2,
            "connection survives a panicking request: {text}"
        );
        // The panicking request is answered with a stable internal error.
        let first: serde_json::Value = serde_json::from_str(frames[0]).expect("frame 0 parses");
        assert_eq!(first["error"]["code"], -32603, "got: {first}");
        assert_eq!(first["id"], 9);
        // The following request is still served normally.
        let second: serde_json::Value = serde_json::from_str(frames[1]).expect("frame 1 parses");
        assert_eq!(second["result"]["ok"], true);
        assert_eq!(second["id"], 10);
    }

    /// A handler that never resolves, to verify the request deadline is
    /// enforced (the loop fails the request instead of blocking forever).
    struct StalledHandler;

    #[async_trait(?Send)]
    impl BridgeHandler for StalledHandler {
        async fn handle(
            &self,
            _request: &BridgeRequest,
        ) -> Result<Option<BridgeResponse>, BridgeError> {
            std::future::pending::<()>().await;
            unreachable!("handler never resolves");
        }
    }

    #[tokio::test]
    async fn stalled_request_is_failed_at_the_deadline() {
        let input = r#"{"jsonrpc":"2.0","method":"session.open","id":3}
"#;
        let mut out = Vec::new();
        // The deadline is injectable, so we use a short one (200 ms) instead
        // of the 30 s v1 default to keep the test fast. A handler that never
        // resolves must be failed with a stable timeout error frame (code
        // 1010) rather than blocking the loop forever.
        run(
            Cursor::new(input),
            &mut out,
            &StalledHandler,
            short_deadline(),
        )
        .await
        .expect("run completes");
        let text = String::from_utf8(out).expect("utf8");
        assert!(
            text.contains("\"code\":1010"),
            "expected a timeout error frame (code 1010), got: {text}"
        );
        assert!(text.contains("\"id\":3"));
    }

    /// A handler that returns an oversized result, to verify the result-size
    /// cap is enforced fail-closed (no oversized NDJSON frame on stdout).
    struct BigResultHandler;

    #[async_trait(?Send)]
    impl BridgeHandler for BigResultHandler {
        async fn handle(
            &self,
            request: &BridgeRequest,
        ) -> Result<Option<BridgeResponse>, BridgeError> {
            let big = "x".repeat(super::super::protocol::MAX_RESULT_BYTES + 1);
            Ok(Some(BridgeResponse {
                jsonrpc: JSONRPC_VERSION,
                result: Some(serde_json::json!({ "blob": big })),
                error: None,
                id: request.id.clone().unwrap_or(Value::Null),
            }))
        }
    }

    #[tokio::test]
    async fn oversized_result_is_refused_fail_closed() {
        let input = r#"{"jsonrpc":"2.0","method":"history.search","id":4}
"#;
        let mut out = Vec::new();
        run(
            Cursor::new(input),
            &mut out,
            &BigResultHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        let frames: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(frames.len(), 1);
        let frame: serde_json::Value = serde_json::from_str(frames[0]).expect("frame parses");
        assert_eq!(frame["error"]["code"], 1001, "frame-too-large: {frame}");
        assert_eq!(frame["id"], 4);
        assert!(
            text.len() < super::super::protocol::MAX_RESULT_BYTES,
            "no oversized line is emitted"
        );
    }

    /// A notification (no `id`) must never receive a reply frame, even when
    /// the method is unknown — JSON-RPC 2.0 requires notifications to be
    /// silent (and GC-LB-05 forbids inventing a response for one).
    #[tokio::test]
    async fn notification_gets_no_reply() {
        // `status.get` without an id, then a normal request with an id.
        let input = r#"{"jsonrpc":"2.0","method":"status.get"}
{"jsonrpc":"2.0","method":"status.get","id":1}
"#;
        let mut out = Vec::new();
        run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        let frames: Vec<&str> = text
            .trim_end()
            .split('\n')
            .filter(|s| !s.is_empty())
            .collect();
        // Only the id-bearing request gets a frame.
        assert_eq!(frames.len(), 1, "notification must be silent: {text}");
        let frame: serde_json::Value = serde_json::from_str(frames[0]).expect("frame parses");
        assert_eq!(frame["id"], 1);
    }

    /// An unterminated line larger than the frame cap must still be refused
    /// fail-closed (the read is bounded even without a trailing newline).
    #[tokio::test]
    async fn unterminated_oversized_line_is_refused() {
        // No trailing newline: the whole payload is one oversized unterminated
        // line, exceeding the cap. The transport must refuse it without
        // buffering it unboundedly.
        let huge = "z".repeat(super::super::protocol::MAX_FRAME_BYTES + 10);
        let input = format!(r#"{{"jsonrpc":"2.0","method":"initialize","id":1,"pad":"{huge}"}}"#);
        let mut out = Vec::new();
        run(
            Cursor::new(input.as_bytes()),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        let frame: serde_json::Value = serde_json::from_str(text.trim_end()).expect("frame parses");
        assert_eq!(frame["error"]["code"], 1001, "frame-too-large: {frame}");
        // The response itself is well under the cap.
        assert!(
            text.len() < super::super::protocol::MAX_FRAME_BYTES,
            "no oversized response line"
        );
    }

    /// A duplicate request id on one connection is rejected fail-closed
    /// (GC-LB-05): the second occurrence gets a stable error, not a silent
    /// second answer.
    #[tokio::test]
    async fn duplicate_request_id_is_rejected() {
        let input = r#"{"jsonrpc":"2.0","method":"status.get","id":7}
{"jsonrpc":"2.0","method":"status.get","id":7}
"#;
        let mut out = Vec::new();
        run(
            Cursor::new(input),
            &mut out,
            &HandshakeHandler,
            test_deadline(),
        )
        .await
        .expect("runs");
        let text = String::from_utf8(out).expect("utf8");
        let frames: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(frames.len(), 2, "both frames produced: {text}");
        // First: status.get is allowlisted but not implemented by LB-01's
        // HandshakeHandler -> not-implemented error frame.
        let first: serde_json::Value = serde_json::from_str(frames[0]).expect("frame 0");
        assert_eq!(first["id"], 7);
        // Second: duplicate id -> invalid-request error frame.
        let second: serde_json::Value = serde_json::from_str(frames[1]).expect("frame 1");
        assert_eq!(second["id"], 7);
        assert_eq!(second["error"]["code"], -32600, "invalid-request: {second}");
    }
}
