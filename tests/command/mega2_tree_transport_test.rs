//! plan-20260912 MB-01: mega2 `/api/v1/tree` transport integration.
//!
//! End-to-end (mock server) checks: query encoding, no Authorization header,
//! valid listing, HTTP failure mapping, and zero local filesystem writes.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    net::{SocketAddr, TcpListener as StdTcpListener},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use axum::{
    Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use libra::{
    internal::protocol::mega2_tree::{ContentType, Mega2TreeClient},
    utils::test::ChangeDirGuard,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Debug, Default, Deserialize)]
struct TreeQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    refs: String,
}

#[derive(Default)]
struct MockState {
    queries: Vec<(String, String)>,
    saw_authorization: bool,
}

/// Spawns an axum mock of the mega2 tree route; returns the bound address,
/// the captured request log, and a handle that stops on drop.
struct MockMega2Tree {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
    _handle: JoinHandle<()>,
}

impl MockMega2Tree {
    async fn start(status: u16, tree_items: serde_json::Value) -> Self {
        let state = Arc::new(Mutex::new(MockState::default()));
        let app_state = Arc::clone(&state);
        let app = Router::new()
            .route(
                "/api/v1/tree",
                get(move |Query(q): Query<TreeQuery>, headers: HeaderMap| {
                    let s = Arc::clone(&app_state);
                    async move {
                        {
                            let mut s = s.lock().expect("mock state poisoned");
                            s.queries.push((q.path.clone(), q.refs.clone()));
                            if headers.contains_key("authorization") {
                                s.saw_authorization = true;
                            }
                        }
                        let body = json!({
                            "req_result": status == 200,
                            "data": if status == 200 {
                                json!({
                                    "tree_items": tree_items,
                                    "file_tree": {"/": {"total_count": 1, "tree_items": [{"name": "ancestor", "path": "/x", "content_type": "file"}]}},
                                })
                            } else {
                                serde_json::Value::Null
                            },
                            "err_message": if status == 200 { "" } else { "mock failure" },
                        });
                        (StatusCode::from_u16(status).expect("valid status"), body.to_string())
                    }
                }),
            )
            .with_state(());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock mega2 tree");
        let addr = listener.local_addr().expect("mock mega2 tree addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            addr,
            state,
            _handle: handle,
        }
    }

    fn url(&self) -> String {
        format!("http://{addr}", addr = self.addr)
    }
}

#[tokio::test]
async fn transport_encodes_query_sends_no_auth_and_returns_sorted_listing() {
    let mock = MockMega2Tree::start(
        200,
        json!([
            {"name": "zeta-file", "path": "/", "content_type": "file"},
            {"name": "beta-dir", "path": "/", "content_type": "directory"},
            {"name": "alpha-file", "path": "/", "content_type": "file"},
        ]),
    )
    .await;

    let client = Mega2TreeClient::new(&mock.url()).expect("client from mock URL");
    let listing = client
        .fetch_listing("/src sub", Some("ref with space"))
        .await
        .expect("listing");

    let state = mock.state.lock().expect("mock state poisoned");
    assert_eq!(state.queries.len(), 1, "exactly one request");
    assert_eq!(state.queries[0].0, "/src sub", "path round-trips");
    assert_eq!(state.queries[0].1, "ref with space", "refs round-trips");
    assert!(!state.saw_authorization, "listing must be anonymous");
    drop(state);

    let names: Vec<(&str, ContentType)> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.content_type))
        .collect();
    assert_eq!(
        names,
        vec![
            ("beta-dir", ContentType::Directory),
            ("alpha-file", ContentType::File),
            ("zeta-file", ContentType::File),
        ]
    );
}

#[tokio::test]
async fn transport_maps_http_failures_and_never_writes_local_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = ChangeDirGuard::new(dir.path());

    let mock_401 = MockMega2Tree::start(401, json!([])).await;
    let err = Mega2TreeClient::new(&mock_401.url())
        .expect("client")
        .fetch_listing("/", None)
        .await
        .expect_err("401 must fail");
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::AuthPermissionDenied
    );
    assert!(
        !err.message().contains("mock failure"),
        "body must not leak"
    );

    let mock_500 = MockMega2Tree::start(500, json!([])).await;
    let err = Mega2TreeClient::new(&mock_500.url())
        .expect("client")
        .fetch_listing("/", None)
        .await
        .expect_err("500 must fail");
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::NetworkProtocol
    );

    // The transport must not write anything into the current directory.
    let leftovers: HashMap<_, _> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .map(|name| (name, ()))
        .collect();
    assert!(
        leftovers.is_empty(),
        "transport wrote local state: {leftovers:?}"
    );
}

// ---- plan-20261001 MN-01: machine-readable failure details ----
//
// One raw-socket fixture per gate. The client must annotate the error with the
// fixed method and route template and with the status it actually received:
// at the first status branch (500), while the body is read (a 200 whose body
// is cut short) and at the last check on a 2xx response (201, so the detail
// cannot be a fixed 200). Route gates also check the request line the mock
// received, so the reported route is the route that was requested.

/// Serves exactly one connection: reads the whole request, reports its request
/// line, writes `response` verbatim and closes. A body shorter than its
/// `Content-Length` makes the client's body read fail after the status arrived.
fn serve_raw_once(response: String) -> (SocketAddr, std::sync::mpsc::Receiver<String>) {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind raw mock");
    let addr = listener.local_addr().expect("raw mock addr");
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept()
            && let Some(line) = read_request_line(&mut stream)
        {
            let _ = line_tx.send(line);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (addr, line_rx)
}

fn received_line(line_rx: &std::sync::mpsc::Receiver<String>) -> String {
    line_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the mock received one request")
}

fn raw_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A 200 whose body stops 64 bytes short of its `Content-Length`.
fn truncated_response(partial: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{partial}",
        partial.len() + 64
    )
}

fn expected_details(method: &str, route: &str, status: u16) -> BTreeMap<String, serde_json::Value> {
    serde_json::json!({"method": method, "route": route, "http_status": status})
        .as_object()
        .expect("details object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Reads one request head plus its `Content-Length` body and returns the
/// request line; `None` on EOF or error.
fn read_request_line(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        bytes.extend_from_slice(&chunk[..n]);
        let Some(head_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
        let body_len = head
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if bytes.len() >= head_end + 4 + body_len {
            return head.lines().next().map(str::to_string);
        }
    }
}

/// Fetches `/` against one raw fixture; returns the error and the request line.
async fn tree_error(response: String) -> (libra::utils::error::CliError, String) {
    let (addr, line_rx) = serve_raw_once(response);
    let err = Mega2TreeClient::new(&format!("http://{addr}"))
        .expect("client")
        .fetch_listing("/", None)
        .await
        .expect_err("fixture must fail");
    (err, received_line(&line_rx))
}

/// MN-01 G7: the first status branch is annotated with the route requested.
#[tokio::test]
async fn failure_details_tree_route() {
    let (err, line) = tree_error(raw_response("500 Internal Server Error", "")).await;
    assert!(
        line.starts_with("GET /api/v1/tree?"),
        "request line: {line}"
    );
    assert_eq!(err.details(), &expected_details("GET", "/api/v1/tree", 500));
}

/// MN-01 G15: `validate_and_sort`, the last check on a 2xx listing, is
/// annotated with the status received.
#[tokio::test]
async fn failure_details_tree_last_stage() {
    let body = json!({
        "req_result": true,
        "data": {"tree_items": [{"name": "..", "path": "/", "content_type": "directory"}]},
        "err_message": "",
    })
    .to_string();
    let (err, _) = tree_error(raw_response("201 Created", &body)).await;
    assert_eq!(err.details(), &expected_details("GET", "/api/v1/tree", 201));
}

/// MN-01 G30: a connection that drops while the body is read carries the
/// status that arrived. The body-read failure keeps its own stable code, which
/// differs from the parse failure a complete invalid body would produce.
#[tokio::test]
async fn failure_details_tree_mid_stream() {
    let (err, _) = tree_error(truncated_response(
        r#"{"req_result":true,"data":{"tree_items":["#,
    ))
    .await;
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::NetworkUnavailable
    );
    assert_eq!(err.details(), &expected_details("GET", "/api/v1/tree", 200));
}
