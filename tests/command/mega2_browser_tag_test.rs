//! plan-20260912 MB-11: TUI tag panel on `mega2 browser`.
//!
//! Pins the panel contract: `t` opens and fetches page 1 only, paging is
//! explicit (one GET per key), create collects name + optional message
//! (empty = lightweight), delete requires confirmation, hostile names never
//! reach the network, list stays anonymous while writes use the session
//! token, and closing the panel restores the directory view.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use libra::{
    command::mega2_browser::{
        ActionResult, BrowserState, Key, TagEditor, TagPanel, perform_create_tag,
        perform_delete_tag, perform_fetch_tags, render,
    },
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_tag::Mega2TagClient,
        mega2_tree::{ContentType, Mega2TreeSession},
    },
};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    target: String,
    headers: std::collections::HashMap<String, String>,
    body: serde_json::Value,
}

fn read_request(stream: &mut TcpStream) -> Option<Captured> {
    const MAX_REQUEST_BYTES: usize = 32 * 1024;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(end) = buf.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end;
        }
        let remaining = MAX_REQUEST_BYTES.checked_sub(buf.len())?;
        if remaining == 0 {
            return None;
        }
        let read_len = remaining.min(chunk.len());
        let n = stream.read(&mut chunk[..read_len]).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = std::collections::HashMap::new();
    for header in lines {
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .ok()?
        .unwrap_or(0);
    let body_start = header_end + 4;
    let body_end = body_start.checked_add(content_length)?;
    if body_end > MAX_REQUEST_BYTES {
        return None;
    }
    while buf.len() < body_end {
        let remaining = MAX_REQUEST_BYTES - buf.len();
        let read_len = remaining.min(chunk.len());
        let n = stream.read(&mut chunk[..read_len]).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body =
        serde_json::from_slice(&buf[body_start..body_end]).unwrap_or(serde_json::Value::Null);
    Some(Captured {
        method,
        target,
        headers,
        body,
    })
}

struct MockTagServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    captured: Arc<Mutex<Vec<Captured>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockTagServer {
    fn start(total: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let last_clone = Arc::clone(&captured);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stream.set_nonblocking(false).is_err()
                            || stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .is_err()
                            || stream
                                .set_write_timeout(Some(Duration::from_secs(5)))
                                .is_err()
                        {
                            continue;
                        }
                        let Some(captured) = read_request(&mut stream) else {
                            continue;
                        };
                        let method = captured.method.clone();
                        let target = captured.target.clone();
                        last_clone.lock().expect("lock").push(captured);
                        requests_clone.fetch_add(1, Ordering::SeqCst);

                        let payload = if method == "GET" && target.contains("/list") {
                            serde_json::json!({
                                "req_result": true,
                                "data": {
                                    "total": total,
                                    "items": [{
                                        "name": "v1.0.0",
                                        "tag_id": "t1",
                                        "object_id": "o1",
                                        "object_type": "commit",
                                        "tagger": "Libra",
                                        "message": "release",
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
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
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
            captured,
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

    fn all(&self) -> Vec<Captured> {
        self.captured.lock().expect("lock").clone()
    }

    fn last(&self) -> Captured {
        self.all().pop().expect("captured")
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

fn state(server: &MockTagServer) -> BrowserState {
    // The directory session is never used by the tag panel tests.
    let session = Mega2TreeSession::new(&server.url()).expect("session");
    BrowserState::new(session, "/", None).expect("state")
}

fn client(server: &MockTagServer, with_token: bool) -> Mega2TagClient {
    let token = with_token.then(|| Mega2Token::new("tok").expect("token"));
    Mega2TagClient::new(&server.url(), token).expect("client")
}

#[test]
fn t_opens_the_panel_and_asks_for_page_one_only() {
    let server = MockTagServer::start(45);
    let mut state = state(&server);

    assert_eq!(
        state.handle_key(Key::Tags),
        ActionResult::FetchTags { page: 1 }
    );
    assert!(state.panel.is_some(), "panel opened");
    assert_eq!(server.requests(), 0, "opening itself does not fetch");

    // Explicit next/previous only.
    if let Some(panel) = state.panel.as_mut() {
        panel.apply_page(1, 45, vec![]);
    }
    assert_eq!(
        state.handle_key(Key::Other('n')),
        ActionResult::FetchTags { page: 2 }
    );
    if let Some(panel) = state.panel.as_mut() {
        panel.apply_page(2, 45, vec![]);
    }
    assert_eq!(
        state.handle_key(Key::Other('p')),
        ActionResult::FetchTags { page: 1 }
    );

    // Panel keys are modal: directory keys are inert while it is open.
    assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
    assert_eq!(state.handle_key(Key::Create), ActionResult::Continue);
    assert!(matches!(
        state.panel.as_ref().and_then(|p| p.editor.as_ref()),
        Some(TagEditor::CreateName(_))
    ));
}

#[tokio::test]
async fn open_fetch_is_one_token_bearing_get() {
    let server = MockTagServer::start(1);
    let tag_client = client(&server, true);
    let mut state = state(&server);
    state.panel = Some(TagPanel::new());

    perform_fetch_tags(&mut state, &tag_client, 1)
        .await
        .expect("page 1");
    assert_eq!(server.requests(), 1, "exactly one page fetch");
    let captured = server.last();
    assert_eq!(captured.method, "GET");
    for key in ["page=1", "per_page=20", "path=%2F"] {
        assert!(captured.target.contains(key), "{}", captured.target);
    }
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tok"),
        "list carries the optional session token"
    );
    let panel = state.panel.as_ref().expect("panel");
    assert_eq!(panel.total, 1);
    assert_eq!(panel.items[0].name, "v1.0.0");
}

#[tokio::test]
async fn create_flow_posts_once_then_refreshes_the_page() {
    let server = MockTagServer::start(1);
    let tag_client = client(&server, true);
    let mut state = state(&server);
    state.panel = Some(TagPanel::new());
    perform_fetch_tags(&mut state, &tag_client, 1)
        .await
        .expect("initial page");
    assert_eq!(server.requests(), 1);

    // Drive the modal flow: `a`, name, Enter, message, Enter.
    state.handle_key(Key::Create);
    for ch in "v2.0.0".chars() {
        state.handle_key(Key::Other(ch));
    }
    assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
    for ch in "release".chars() {
        state.handle_key(Key::Other(ch));
    }
    let action = state.handle_key(Key::Enter);
    let ActionResult::CreateTag { name, message } = action else {
        panic!("expected CreateTag, got {action:?}");
    };
    assert_eq!(name, "v2.0.0");
    assert_eq!(message.as_deref(), Some("release"));

    perform_create_tag(&mut state, &tag_client, &name, message.as_deref())
        .await
        .expect("create succeeds");
    assert_eq!(
        server.requests(),
        3,
        "1 initial GET + 1 POST + 1 refresh GET"
    );
    let captured = server.last();
    assert_eq!(captured.method, "GET", "refresh is a list GET");
    let post = server
        .all()
        .into_iter()
        .find(|request| request.method == "POST")
        .expect("create POST");
    assert!(post.target.starts_with("/api/v1/tags"), "{}", post.target);
    assert_eq!(post.body["name"], "v2.0.0");
    assert_eq!(post.body["message"], "release");
    assert!(post.body.get("tagger").is_none(), "{}", post.body);
    assert_eq!(
        post.headers.get("authorization").map(String::as_str),
        Some("Bearer tok"),
        "writes use the session token"
    );
}

#[tokio::test]
async fn delete_flow_requires_confirmation_then_deletes_and_refreshes() {
    let server = MockTagServer::start(1);
    let tag_client = client(&server, true);
    let mut state = state(&server);
    state.panel = Some(TagPanel::new());
    perform_fetch_tags(&mut state, &tag_client, 1)
        .await
        .expect("page");
    if let Some(panel) = state.panel.as_mut() {
        panel.selection = 0;
    }

    // `d` opens the confirmation; Esc cancels without any request.
    assert_eq!(state.handle_key(Key::Delete), ActionResult::Continue);
    assert!(matches!(
        state.panel.as_ref().and_then(|p| p.editor.as_ref()),
        Some(TagEditor::ConfirmDelete(_))
    ));
    assert_eq!(state.handle_key(Key::Cancel), ActionResult::Continue);
    assert_eq!(server.requests(), 1, "cancel sends nothing");

    // Confirm deletes and refreshes.
    state.handle_key(Key::Delete);
    let action = state.handle_key(Key::Enter);
    let ActionResult::DeleteTag { name } = action else {
        panic!("expected DeleteTag, got {action:?}");
    };
    assert_eq!(name, "v1.0.0");
    perform_delete_tag(&mut state, &tag_client, &name)
        .await
        .expect("delete succeeds");
    assert_eq!(server.requests(), 3, "GET + DELETE + refresh GET");
    let delete = server
        .all()
        .into_iter()
        .find(|request| request.method == "DELETE")
        .expect("delete request");
    assert!(delete.target.contains("path=%2F"), "{}", delete.target);
    assert_eq!(
        delete.headers.get("authorization").map(String::as_str),
        Some("Bearer tok")
    );
}

#[test]
fn hostile_names_never_leave_the_editor() {
    let server = MockTagServer::start(1);
    let mut state = state(&server);
    state.panel = Some(TagPanel::new());
    if let Some(panel) = state.panel.as_mut() {
        panel.editor = Some(TagEditor::CreateName("a..b".to_string()));
    }
    assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
    assert_eq!(server.requests(), 0, "no request for a hostile name");
    let panel = state.panel.as_ref().expect("panel");
    assert!(panel.status.is_some());
    assert!(matches!(panel.editor, Some(TagEditor::CreateName(_))));
}

#[test]
fn closing_the_panel_restores_the_directory_view() {
    let server = MockTagServer::start(1);
    let mut state = state(&server);
    state.entries = vec![
        ("sub".to_string(), ContentType::Directory),
        ("readme.txt".to_string(), ContentType::File),
    ];
    let action = state.handle_key(Key::Tags);
    assert_eq!(action, ActionResult::FetchTags { page: 1 });
    let frame = render(&state, "http://127.0.0.1:1");
    assert!(frame.contains("mega2 tags"), "{frame}");

    assert_eq!(state.handle_key(Key::Cancel), ActionResult::Continue);
    assert!(state.panel.is_none(), "Esc closes the panel");
    let frame = render(&state, "http://127.0.0.1:1");
    assert!(frame.contains("sub"), "directory listing restored: {frame}");
    assert!(frame.contains("readme.txt"), "{frame}");
    assert!(
        state.running,
        "the browser keeps running after closing the panel"
    );
}

#[test]
fn panel_renderer_escapes_control_characters() {
    let mut panel = TagPanel::new();
    panel.apply_page(
        1,
        1,
        vec![
            serde_json::from_value(serde_json::json!({
                "name": "v1",
                "tag_id": "t",
                "object_id": "o",
                "object_type": "commit",
                "tagger": "tagger\u{1b}[2J",
                "message": "line1\u{7}\nline2",
                "created_at": "now",
            }))
            .expect("tag"),
        ],
    );
    let rendered = panel.render_lines();
    assert!(!rendered.contains('\u{1b}'), "ESC escaped: {rendered}");
    assert!(!rendered.contains('\u{7}'), "bell escaped: {rendered}");
}

fn libra(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("failed to spawn libra binary")
}

#[test]
fn help_has_no_tag_subcommand() {
    let workdir = tempfile::tempdir().expect("tempdir");
    let parent = libra(workdir.path(), &["mega2", "--help"]);
    assert!(parent.status.success());
    let help = String::from_utf8_lossy(&parent.stdout);
    assert!(!help.contains("mega2 tag"), "{help}");
    assert!(!help.contains("tag "), "no tag subcommand: {help}");
}
