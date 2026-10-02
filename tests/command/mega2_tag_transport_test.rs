//! plan-20260912 MB-10: `tag_router` transport contract.
//!
//! Mock family: list page (GET + required query keys, anonymous), create
//! lightweight vs annotated, delete, 401 on create, 403 on a path-scoped
//! token for create and delete, hostile name, and the wrong-method
//! POST-list rejection (405) proving the client only ever uses GET.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use libra::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_tag::{CreateTagOptions, Mega2TagClient},
    },
    utils::error::StableErrorCode,
};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: serde_json::Value,
}

/// Routes tag requests: methodology checks are done by the handler closure.
struct MockTagServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    last: Arc<Mutex<Option<Captured>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    const MAX_REQUEST_BYTES: usize = 64 * 1024;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 || bytes.len() + n > MAX_REQUEST_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
        let Some(head_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = std::str::from_utf8(&bytes[..head_end]).ok()?;
        let content_length = head
            .split("\r\n")
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse::<usize>())
            .transpose()
            .ok()?
            .unwrap_or(0);
        let request_end = head_end.checked_add(4)?.checked_add(content_length)?;
        if request_end > MAX_REQUEST_BYTES {
            return None;
        }
        if bytes.len() >= request_end {
            return String::from_utf8(bytes).ok();
        }
    }
}

impl MockTagServer {
    /// `respond` maps (method, path) to a status code; 200 sends a valid body.
    fn start(respond: fn(&str, &str) -> u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let last_clone = Arc::clone(&last);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking mock connection");
                        let Some(raw) = read_request(&mut stream) else {
                            continue;
                        };
                        let (head, raw_body) = raw
                            .split_once("\r\n\r\n")
                            .map(|(h, b)| (h.to_string(), b.to_string()))
                            .unwrap_or_default();
                        let mut lines = head.split("\r\n");
                        let line = lines.next().unwrap_or_default().to_string();
                        let mut parts = line.split_whitespace();
                        let method = parts.next().unwrap_or_default().to_string();
                        let target = parts.next().unwrap_or_default().to_string();
                        let path = target.split('?').next().unwrap_or_default().to_string();
                        let mut headers = HashMap::new();
                        for header in lines {
                            if let Some((name, value)) = header.split_once(':') {
                                headers.insert(
                                    name.trim().to_ascii_lowercase(),
                                    value.trim().to_string(),
                                );
                            }
                        }
                        let body =
                            serde_json::from_str(&raw_body).unwrap_or(serde_json::Value::Null);
                        *last_clone.lock().expect("lock") = Some(Captured {
                            method: method.clone(),
                            target: target.clone(),
                            headers,
                            body: body.clone(),
                        });
                        requests_clone.fetch_add(1, Ordering::SeqCst);

                        let status = respond(&method, &path);
                        let payload = if status == 200 {
                            if method == "GET" && path.ends_with("/list") {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {
                                        "total": 2,
                                        "items": [{
                                            "name": "v1.0.0",
                                            "tag_id": "t1",
                                            "object_id": "o1",
                                            "object_type": "commit",
                                            "tagger": "Libra",
                                            "message": "",
                                            "created_at": "2026-09-21T00:00:00Z",
                                        }],
                                    },
                                })
                            } else if method == "DELETE" {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {"deleted_tag": "v1.0.0", "message": "deleted"},
                                })
                            } else {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {
                                        "name": "v1.0.0",
                                        "tag_id": "t1",
                                        "object_id": "o1",
                                        "object_type": "commit",
                                        "tagger": "Libra",
                                        "message": "release",
                                        "created_at": "2026-09-21T00:00:00Z",
                                    },
                                })
                            }
                        } else {
                            serde_json::json!({"req_result": false, "data": null})
                        }
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            last,
            stop,
            join: Some(join),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn last(&self) -> Captured {
        self.last.lock().expect("lock").clone().expect("captured")
    }
}

impl Drop for MockTagServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn ok_respond(_method: &str, _path: &str) -> u16 {
    200
}

fn client(server: &MockTagServer, with_token: bool) -> Mega2TagClient {
    let token = with_token.then(|| Mega2Token::new("tag-token").expect("token"));
    Mega2TagClient::new(&server.url(), token).expect("client")
}

#[tokio::test]
async fn list_uses_get_with_the_three_required_keys_and_no_authorization() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    let page = client.list_tags(1, 50, "/").await.expect("list succeeds");

    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "v1.0.0");
    assert_eq!(page.items[0].tagger, "Libra");

    let captured = server.last();
    assert_eq!(captured.method, "GET", "list is GET, never POST");
    assert!(
        captured.target.starts_with("/api/v1/tags/list?"),
        "{}",
        captured.target
    );
    for key in ["page=1", "per_page=50", "path=%2F"] {
        assert!(captured.target.contains(key), "{}", captured.target);
    }
    assert!(
        !captured.headers.contains_key("authorization"),
        "list is anonymous: {:?}",
        captured.headers
    );
    assert_eq!(server.requests(), 1, "one page per request");
}

#[tokio::test]
async fn create_lightweight_and_annotated_shapes() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);

    let lightweight = CreateTagOptions {
        name: "v1.0.0",
        ..CreateTagOptions::default()
    };
    let tag = client.create_tag(&lightweight).await.expect("lightweight");
    assert_eq!(tag.name, "v1.0.0");
    let captured = server.last();
    assert_eq!(captured.method, "POST");
    assert!(
        captured.target.starts_with("/api/v1/tags"),
        "{}",
        captured.target
    );
    assert_eq!(captured.body["name"], "v1.0.0");
    assert_eq!(captured.body["path_context"], "/");
    assert!(
        captured.body.get("message").is_none(),
        "omitted message = lightweight: {}",
        captured.body
    );
    assert!(captured.body.get("tagger").is_none());
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tag-token")
    );

    let annotated = CreateTagOptions {
        name: "v2.0.0",
        target: Some("deadbeef"),
        tagger_name: Some("Libra"),
        message: Some("release 2.0.0"),
        ..CreateTagOptions::default()
    };
    client.create_tag(&annotated).await.expect("annotated");
    let body = server.last().body;
    assert_eq!(body["message"], "release 2.0.0");
    assert_eq!(body["target"], "deadbeef");
    assert_eq!(body["tagger_name"], "Libra");
    assert!(body.get("tagger").is_none());
    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn delete_uses_the_tag_route_with_the_path_selector() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    let receipt = client.delete_tag("v1.0.0", "/").await.expect("delete");

    assert_eq!(receipt.deleted_tag, "v1.0.0");
    assert_eq!(receipt.message, "deleted");
    let captured = server.last();
    assert_eq!(captured.method, "DELETE");
    assert!(
        captured.target.starts_with("/api/v1/tags/v1.0.0?"),
        "{}",
        captured.target
    );
    assert!(captured.target.contains("path=%2F"), "{}", captured.target);
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tag-token")
    );
}

#[tokio::test]
async fn unauthenticated_create_maps_to_auth_missing_credentials() {
    let server = MockTagServer::start(|_m, _p| 401);
    let client = client(&server, false);
    let err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("401");
    assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
    assert_eq!(server.requests(), 1);
}

#[tokio::test]
async fn path_scoped_token_403_is_diagnosable_for_create_and_delete() {
    let server = MockTagServer::start(|_m, _p| 403);
    let client = client(&server, true);

    let create_err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            path_context: Some("/project"),
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("403 create");
    assert_eq!(
        create_err.stable_code(),
        StableErrorCode::AuthPermissionDenied
    );
    assert!(!create_err.render().contains("tag-token"), "token leaked");

    let delete_err = client.delete_tag("v1", "/").await.expect_err("403 delete");
    assert_eq!(
        delete_err.stable_code(),
        StableErrorCode::AuthPermissionDenied
    );
    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn hostile_names_are_refused_before_any_request() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    for bad in ["..", "a..b", "a//b", "release.lock", "has space", "nul\0"] {
        let err = client
            .create_tag(&CreateTagOptions {
                name: bad,
                ..CreateTagOptions::default()
            })
            .await
            .expect_err("hostile name");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        assert!(client.get_tag(bad, "/").await.is_err());
        assert!(client.delete_tag(bad, "/").await.is_err());
    }
    assert_eq!(server.requests(), 0, "validation precedes the network");
}

#[tokio::test]
async fn list_never_posts_and_wrong_method_405_is_a_stable_error() {
    // Only GET on the list route is accepted; anything else is a 405.
    let server = MockTagServer::start(|method, path| {
        if method == "GET" && path.ends_with("/list") {
            200
        } else {
            405
        }
    });
    let client = client(&server, false);
    client.list_tags(1, 10, "/").await.expect("GET accepted");
    assert_eq!(server.last().method, "GET");

    // A 405 arriving for a write route maps to a stable usage error.
    let err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("405");
    assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
    assert_eq!(server.requests(), 2);
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind raw mock");
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

fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    read_request(stream).and_then(|raw| raw.lines().next().map(str::to_string))
}

/// Runs `call` against a tag client bound to one raw fixture; returns the
/// error and the request line the mock received.
async fn tag_error<F, Fut, T>(response: String, call: F) -> (libra::utils::error::CliError, String)
where
    F: FnOnce(Mega2TagClient) -> Fut,
    Fut: std::future::Future<Output = libra::utils::error::CliResult<T>>,
    T: std::fmt::Debug,
{
    let (addr, line_rx) = serve_raw_once(response);
    let client = Mega2TagClient::new(&format!("http://{addr}"), None).expect("client");
    let err = call(client).await.expect_err("fixture must fail");
    (err, received_line(&line_rx))
}

async fn list_error(response: String) -> (libra::utils::error::CliError, String) {
    tag_error(response, |client| async move {
        client.list_tags(1, 20, "/").await
    })
    .await
}

async fn create_error(response: String) -> (libra::utils::error::CliError, String) {
    tag_error(response, |client| async move {
        client
            .create_tag(&CreateTagOptions {
                name: "v1",
                target: None,
                path_context: None,
                tagger_name: None,
                tagger_email: None,
                message: None,
            })
            .await
    })
    .await
}

async fn get_error(response: String) -> (libra::utils::error::CliError, String) {
    tag_error(
        response,
        |client| async move { client.get_tag("v1", "/").await },
    )
    .await
}

async fn delete_error(response: String) -> (libra::utils::error::CliError, String) {
    tag_error(response, |client| async move {
        client.delete_tag("v1", "/").await
    })
    .await
}

fn status_500() -> String {
    raw_response("500 Internal Server Error", "")
}

/// A 201 whose `data` is not an object, so `finish` cannot parse it.
fn data_not_object_201() -> String {
    raw_response(
        "201 Created",
        r#"{"req_result":true,"data":"not-an-object"}"#,
    )
}

const PARTIAL_TAG_BODY: &str = r#"{"req_result":true,"data":{"name":"#;

/// MN-01 G11.
#[tokio::test]
async fn failure_details_list_tags_route() {
    let (err, line) = list_error(status_500()).await;
    assert!(
        line.starts_with("GET /api/v1/tags/list?"),
        "request line: {line}"
    );
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/list", 500)
    );
}

/// MN-01 G12.
#[tokio::test]
async fn failure_details_create_tag_route() {
    let (err, line) = create_error(status_500()).await;
    assert!(
        line.starts_with("POST /api/v1/tags "),
        "request line: {line}"
    );
    assert_eq!(
        err.details(),
        &expected_details("POST", "/api/v1/tags", 500)
    );
}

/// MN-01 G13: the route is the template, never the tag name.
#[tokio::test]
async fn failure_details_get_tag_route() {
    let (err, line) = get_error(status_500()).await;
    assert!(
        line.starts_with("GET /api/v1/tags/v1?"),
        "request line: {line}"
    );
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/{name}", 500)
    );
}

/// MN-01 G14: the route is the template, never the tag name.
#[tokio::test]
async fn failure_details_delete_tag_route() {
    let (err, line) = delete_error(status_500()).await;
    assert!(
        line.starts_with("DELETE /api/v1/tags/v1?"),
        "request line: {line}"
    );
    assert_eq!(
        err.details(),
        &expected_details("DELETE", "/api/v1/tags/{name}", 500)
    );
}

/// MN-01 G19.
#[tokio::test]
async fn failure_details_list_tags_last_stage() {
    let (err, _) = list_error(data_not_object_201()).await;
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/list", 201)
    );
}

/// MN-01 G20.
#[tokio::test]
async fn failure_details_create_tag_last_stage() {
    let (err, _) = create_error(data_not_object_201()).await;
    assert_eq!(
        err.details(),
        &expected_details("POST", "/api/v1/tags", 201)
    );
}

/// MN-01 G21.
#[tokio::test]
async fn failure_details_get_tag_last_stage() {
    let (err, _) = get_error(data_not_object_201()).await;
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/{name}", 201)
    );
}

/// MN-01 G22.
#[tokio::test]
async fn failure_details_delete_tag_last_stage() {
    let (err, _) = delete_error(data_not_object_201()).await;
    assert_eq!(
        err.details(),
        &expected_details("DELETE", "/api/v1/tags/{name}", 201)
    );
}

/// MN-01 G34: the body-read failure keeps its own stable code.
#[tokio::test]
async fn failure_details_list_tags_mid_stream() {
    let (err, _) = list_error(truncated_response(PARTIAL_TAG_BODY)).await;
    assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/list", 200)
    );
}

/// MN-01 G35.
#[tokio::test]
async fn failure_details_create_tag_mid_stream() {
    let (err, _) = create_error(truncated_response(PARTIAL_TAG_BODY)).await;
    assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    assert_eq!(
        err.details(),
        &expected_details("POST", "/api/v1/tags", 200)
    );
}

/// MN-01 G36.
#[tokio::test]
async fn failure_details_get_tag_mid_stream() {
    let (err, _) = get_error(truncated_response(PARTIAL_TAG_BODY)).await;
    assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    assert_eq!(
        err.details(),
        &expected_details("GET", "/api/v1/tags/{name}", 200)
    );
}

/// MN-01 G37.
#[tokio::test]
async fn failure_details_delete_tag_mid_stream() {
    let (err, _) = delete_error(truncated_response(PARTIAL_TAG_BODY)).await;
    assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    assert_eq!(
        err.details(),
        &expected_details("DELETE", "/api/v1/tags/{name}", 200)
    );
}
