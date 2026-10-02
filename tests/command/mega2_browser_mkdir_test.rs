//! plan-20260912 MB-05: TUI `+` create-directory flow on `mega2 browser`.
//!
//! Pins the editor contract (sanitized input, Esc cancel with zero network,
//! hostile names refused before POST), the success path (exactly one POST +
//! one reload GET), error handling that keeps the last safe listing with a
//! secret-free status, and the CLI boundary (read operations refuse token
//! flags and never echo them, help has no `mkdir` subcommand).

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use libra::{
    command::mega2_browser::{ActionResult, BrowserState, Editor, Key, perform_create},
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_entry::Mega2EntryClient,
        mega2_tree::{ContentType, Listing, ListingEntry, Mega2TreeSession},
    },
};

/// Minimal blocking HTTP mock: fixed status/body, counts requests.
struct MockServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    fn start(status: u16, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 8192];
                        let _ = stream.read(&mut buf);
                        requests_clone.fetch_add(1, Ordering::SeqCst);
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
            stop,
            join: Some(join),
        }
    }

    fn tree() -> Self {
        Self::start(
            200,
            serde_json::json!({
                "req_result": true,
                "data": {"tree_items": [
                    {"name": "sub", "path": "/", "content_type": "directory"},
                    {"name": "readme.txt", "path": "/", "content_type": "file"},
                ]},
                "err_message": "",
            })
            .to_string(),
        )
    }

    fn entry_ok() -> Self {
        Self::start(
            200,
            serde_json::json!({
                "req_result": true,
                "data": {
                    "commit_id": "commit-1",
                    "new_oid": "oid-1",
                    "path": "/newdir",
                    "cl_link": null,
                },
            })
            .to_string(),
        )
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn state_with(server: &MockServer) -> BrowserState {
    let session = Mega2TreeSession::new(&server.url()).expect("session");
    BrowserState::new(session, "/", None).expect("state")
}

fn type_text(state: &mut BrowserState, text: &str) {
    for ch in text.chars() {
        assert_eq!(
            state.handle_key(Key::Other(ch)),
            ActionResult::Continue,
            "typing {ch:?}"
        );
    }
}

#[test]
fn plus_requires_a_non_file_selection() {
    let server = MockServer::tree();
    let mut state = state_with(&server);

    // Empty listing: creating is allowed (parent is the current path).
    assert_eq!(state.handle_key(Key::Create), ActionResult::Continue);
    assert!(matches!(state.editor, Some(Editor::CreateDir(_))));

    // A file selection blocks creation with an explanatory status.
    state.editor = None;
    state.status = None;
    state.entries = vec![
        ("sub".to_string(), ContentType::Directory),
        ("readme.txt".to_string(), ContentType::File),
    ];
    state.selection = 1; // readme.txt
    assert_eq!(state.handle_key(Key::Create), ActionResult::Continue);
    assert!(
        state.editor.is_none(),
        "file selection must not open the editor"
    );
    assert!(
        state
            .status
            .as_deref()
            .unwrap_or_default()
            .contains("directory"),
        "status: {:?}",
        state.status
    );

    // A directory selection opens the editor.
    state.selection = 0;
    assert_eq!(state.handle_key(Key::Create), ActionResult::Continue);
    assert!(matches!(state.editor, Some(Editor::CreateDir(_))));
}

#[test]
fn esc_cancels_without_any_pending_create() {
    let server = MockServer::tree();
    let mut state = state_with(&server);
    state.handle_key(Key::Create);
    type_text(&mut state, "draft");

    assert_eq!(state.handle_key(Key::Cancel), ActionResult::Continue);
    assert!(state.editor.is_none(), "editor closed");
    assert_eq!(state.status.as_deref(), Some("create cancelled"));
    assert_eq!(
        server.requests(),
        0,
        "editor input never touches the network"
    );
}

#[test]
fn hostile_names_are_refused_before_any_post() {
    let server = MockServer::tree();
    let mut state = state_with(&server);

    for bad in ["a/b", "..", "\\x"] {
        state.editor = Some(Editor::CreateDir(String::new()));
        state.status = None;
        type_text(&mut state, bad);
        let action = state.handle_key(Key::Enter);
        assert_eq!(action, ActionResult::Continue, "no create for {bad:?}");
        assert!(state.editor.is_some(), "editor stays open for {bad:?}");
        assert!(state.status.is_some(), "status explains {bad:?}");
    }

    // Control characters never reach the editor (or the renderer).
    state.editor = Some(Editor::CreateDir(String::new()));
    state.handle_key(Key::Other('\u{1b}'));
    state.handle_key(Key::Other('\u{7}'));
    assert!(matches!(state.editor, Some(Editor::CreateDir(ref input)) if input.is_empty()));

    assert_eq!(server.requests(), 0, "no request before confirmation");
}

#[tokio::test]
async fn confirmed_create_posts_once_then_reloads_once() {
    let tree = MockServer::tree();
    let entry = MockServer::entry_ok();
    let mut state = state_with(&tree);
    state.fetch_current().await.expect("initial listing");
    assert_eq!(tree.requests(), 1);

    state.handle_key(Key::Create);
    type_text(&mut state, "newdir");
    let action = state.handle_key(Key::Enter);
    let ActionResult::CreateDirectory { parent, name } = action else {
        panic!("expected CreateDirectory, got {action:?}");
    };
    assert_eq!(parent, "/", "root parent is sent as /");
    assert_eq!(name, "newdir");

    let client =
        Mega2EntryClient::new(&entry.url(), Some(Mega2Token::new("tok").unwrap())).expect("client");
    perform_create(&mut state, &client, &parent, &name)
        .await
        .expect("create succeeds");

    assert_eq!(entry.requests(), 1, "exactly one POST");
    assert_eq!(tree.requests(), 2, "exactly one reload GET");
    assert!(state.status.is_none(), "status cleared by the reload");
}

#[tokio::test]
async fn failure_keeps_last_listing_and_never_leaks_the_token() {
    let tree = MockServer::tree();
    let entry = MockServer::start(401, "SECRET-BODY".to_string());
    let mut state = state_with(&tree);
    state.fetch_current().await.expect("initial listing");
    let before = state.entries.clone();

    let client =
        Mega2EntryClient::new(&entry.url(), Some(Mega2Token::new("super-secret").unwrap()))
            .expect("client");
    let err = perform_create(&mut state, &client, "/", "newdir")
        .await
        .expect_err("401 refuses");
    let rendered = err.render();
    assert!(
        !rendered.contains("super-secret"),
        "token leaked: {rendered}"
    );
    assert!(!rendered.contains("SECRET-BODY"), "body leaked: {rendered}");

    // The caller keeps the last safe listing and renders its message.
    state.status = Some(format!("error: {}", err.message()));
    assert_eq!(state.entries, before);
    assert_eq!(tree.requests(), 1, "no reload after a failed POST");
    let status = state.status.as_deref().unwrap_or_default();
    assert!(!status.contains("super-secret"), "status leaked: {status}");
}

#[test]
fn duplicate_and_forbidden_statuses_are_secret_free() {
    for status in [400, 403, 409] {
        let server = MockServer::start(status, "SECRET-BODY".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let client =
            Mega2EntryClient::new(&server.url(), Some(Mega2Token::new("tok-123").unwrap()))
                .expect("client");
        let err = runtime
            .block_on(client.create_directory("/", "dir"))
            .expect_err("refused");
        let rendered = err.render();
        assert!(!rendered.contains("SECRET-BODY"), "{rendered}");
        assert!(!rendered.contains("tok-123"), "{rendered}");
        assert_eq!(server.requests(), 1);
    }
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
fn token_flags_are_refused_for_reads_and_never_echoed() {
    let workdir = tempfile::tempdir().expect("tempdir");

    let json = libra(
        workdir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            "http://127.0.0.1:9",
            "--json",
            "--token",
            "secret",
        ],
    );
    assert!(!json.status.success(), "token + --json must be refused");
    let err = String::from_utf8_lossy(&json.stderr).to_string();
    assert!(err.contains("no credentials"), "unexpected stderr: {err}");
    assert!(!err.contains("secret"), "token leaked: {err}");

    // In human mode the token flag is accepted; the failure is the TTY gate.
    let human = libra(
        workdir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            "http://127.0.0.1:9",
            "--token-file",
            "/nonexistent/token",
        ],
    );
    assert!(!human.status.success());
    let err = String::from_utf8_lossy(&human.stderr).to_string();
    assert!(
        err.contains("terminal") || err.contains("token file"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn help_has_token_flags_and_no_mkdir_subcommand() {
    let workdir = tempfile::tempdir().expect("tempdir");

    let browser = libra(workdir.path(), &["mega2", "browser", "--help"]);
    assert!(browser.status.success());
    let browser_help = String::from_utf8_lossy(&browser.stdout);
    assert!(browser_help.contains("--token-file"), "{browser_help}");
    assert!(browser_help.contains("--token"), "{browser_help}");
    assert!(browser_help.contains("EXAMPLES:"), "{browser_help}");
    assert!(!browser_help.contains("mkdir"), "{browser_help}");

    let parent = libra(workdir.path(), &["mega2", "--help"]);
    assert!(parent.status.success());
    let parent_help = String::from_utf8_lossy(&parent.stdout);
    assert!(!parent_help.contains("mkdir"), "{parent_help}");
    assert!(!parent_help.contains("rmdir"), "{parent_help}");
}

#[tokio::test]
async fn listing_types_are_pinned_for_the_tui_targets() {
    // Guards the entries the editor decisions rely on (dir vs file).
    let listing = Listing {
        entries: vec![
            ListingEntry {
                name: "sub".to_string(),
                content_type: ContentType::Directory,
            },
            ListingEntry {
                name: "readme.txt".to_string(),
                content_type: ContentType::File,
            },
        ],
    };
    assert_eq!(listing.entries[0].content_type, ContentType::Directory);
    assert_eq!(listing.entries[1].content_type, ContentType::File);
}
