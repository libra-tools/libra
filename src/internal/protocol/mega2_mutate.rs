//! Bounded `POST /api/v1/{delete-entry,move-entry}` directory mutation client
//! (plan-20260912 MB-07, DEP-MB-04).
//!
//! The client turns an untrusted directory delete/move response into an
//! immutable receipt. It only ever calls the two mounted routes, sends
//! `is_directory`/`author_username` as omitted (the server defaults
//! `is_directory` to `true`), reuses [`Mega2Token`] for exactly one
//! `Authorization: Bearer` header, and performs exactly one request per
//! operation — no recursion, no child enumeration, no local repository or
//! filesystem writes.
//!
//! ## Start gate (AC-1)
//!
//! Construction is refused unless the caller presents evidence that the
//! DEP-MB-04 mounted pin (`mega2@a1293686`, `preview_router.rs:124-183`) was
//! re-verified for this build, including that move's `push_auth` uses the same
//! kind as create-entry. [`MountedPin::verified_a1293686`] is that evidence;
//! [`Mega2MutateClient::with_pin`] is the test seam that proves a mismatched
//! pin cannot start the client.

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use super::mega2_diag;
use crate::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_entry::validate_entry_name,
        mega2_tree::{
            ContentType, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, normalize_path, validate_server_url,
        },
    },
    utils::error::{CliError, CliResult, StableErrorCode},
};

/// Mounted delete route (parent `path` + `name`).
pub const DELETE_ENTRY_ROUTE: &str = "/api/v1/delete-entry";
/// Mounted move/rename route (source and destination parents).
pub const MOVE_ENTRY_ROUTE: &str = "/api/v1/move-entry";
/// The pinned DEP-MB-04 evidence this module was written against.
pub const DEP_MB_04_REFERENCE: &str = "mega2@a1293686 preview_router.rs:124-183";

/// Re-verified state of the mounted DEP-MB-04 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountedPin {
    /// Pin the verification was performed against.
    pub reference: &'static str,
    /// `POST /api/v1/delete-entry` is mounted on the shared write routers.
    pub delete_route_mounted: bool,
    /// `POST /api/v1/move-entry` is mounted on the shared write routers.
    pub move_route_mounted: bool,
    /// Move's `push_auth` gate is the same kind as create-entry's
    /// (`trunk_write_requester` authorizing both parents).
    pub push_auth_same_kind_as_create_entry: bool,
}

impl MountedPin {
    /// The verification performed for this card (ER-MB-02, 2026-09-21).
    pub fn verified_a1293686() -> Self {
        Self {
            reference: DEP_MB_04_REFERENCE,
            delete_route_mounted: true,
            move_route_mounted: true,
            push_auth_same_kind_as_create_entry: true,
        }
    }

    /// Whether this evidence satisfies the start gate.
    pub fn is_verified(&self) -> bool {
        self.reference == DEP_MB_04_REFERENCE
            && self.delete_route_mounted
            && self.move_route_mounted
            && self.push_auth_same_kind_as_create_entry
    }
}

#[derive(Debug, Serialize)]
struct DeleteEntryBody<'a> {
    path: &'a str,
    name: &'a str,
    skip_build: bool,
}

#[derive(Debug, Serialize)]
struct MoveEntryBody<'a> {
    from_path: &'a str,
    from_name: &'a str,
    to_path: &'a str,
    to_name: &'a str,
    skip_build: bool,
}

#[derive(Debug, Deserialize)]
struct WireCommonResult<T> {
    req_result: bool,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct WireDeleteResult {
    commit_id: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    cl_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireMoveResult {
    commit_id: String,
    #[serde(default)]
    from_path: Option<String>,
    #[serde(default)]
    to_path: Option<String>,
    #[serde(default)]
    cl_link: Option<String>,
}

/// What a successful delete proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReceipt {
    /// Commit created by the server (never empty on success).
    pub commit_id: String,
    /// Server-reported deleted path — receipt only, never navigation input.
    pub path: Option<String>,
    /// Server CL link, if any.
    pub cl_link: Option<String>,
}

/// What a successful move/rename proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReceipt {
    /// Commit created by the server (never empty on success).
    pub commit_id: String,
    /// Server-reported source path — receipt only.
    pub from_path: Option<String>,
    /// Server-reported destination path — receipt only.
    pub to_path: Option<String>,
    /// Server CL link, if any.
    pub cl_link: Option<String>,
}

/// The single request body for a delete (AC-2).
fn delete_body<'a>(parent_path: &'a str, name: &'a str) -> DeleteEntryBody<'a> {
    DeleteEntryBody {
        path: parent_path,
        name,
        skip_build: true,
    }
}

/// The single request body for a move (AC-2); rename reuses it unchanged.
fn move_body<'a>(
    from_path: &'a str,
    from_name: &'a str,
    to_path: &'a str,
    to_name: &'a str,
) -> MoveEntryBody<'a> {
    MoveEntryBody {
        from_path,
        from_name,
        to_path,
        to_name,
        skip_build: true,
    }
}

/// Validates a delete target locally and returns the canonical parent path.
///
/// Root deletion is impossible by construction (an entry name is required and
/// validated as a single segment); file-typed targets are refused when the
/// caller already knows the listing type.
fn validate_delete_target(
    parent_path: &str,
    name: &str,
    known_type: Option<ContentType>,
) -> CliResult<String> {
    let parent = normalize_path(parent_path)?;
    validate_entry_name(name)?;
    if known_type == Some(ContentType::File) {
        return Err(CliError::fatal(
            "mega2 mutate: file targets are not supported; only directories can be deleted",
        )
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok(parent)
}

/// Validates a move/rename locally and returns the canonical parents.
fn validate_move_target(
    from_parent: &str,
    from_name: &str,
    to_parent: &str,
    to_name: &str,
    known_type: Option<ContentType>,
    overwrite: bool,
) -> CliResult<(String, String)> {
    if overwrite {
        // The mounted contract has no overwrite mode; destination collisions
        // must fail closed (default refuse) instead of silently replacing.
        return Err(CliError::fatal(
            "mega2 mutate: overwrite is not supported by the mounted contract",
        )
        .with_stable_code(StableErrorCode::Unsupported));
    }
    let from = normalize_path(from_parent)?;
    let to = normalize_path(to_parent)?;
    validate_entry_name(from_name)?;
    validate_entry_name(to_name)?;
    if known_type == Some(ContentType::File) {
        return Err(CliError::fatal(
            "mega2 mutate: file targets are not supported; only directories can be moved",
        )
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok((from, to))
}

/// Maps a status + body into the shared envelope, secret-free.
fn parse_common_envelope<T: DeserializeOwned>(
    status: u16,
    raw: &[u8],
    operation: &str,
) -> CliResult<T> {
    match status {
        401 => {
            return Err(CliError::fatal(format!(
                "mega2 server requires a write token for {operation} (HTTP 401)"
            ))
            .with_stable_code(StableErrorCode::AuthMissingCredentials)
            .with_hint(
                "provide a token with --token-file <path> (or LIBRA_MEGA2_TOKEN) allowed to write these paths",
            ));
        }
        403 => {
            return Err(CliError::fatal(format!(
                "mega2 write token is not authorized for one of the {operation} paths (HTTP 403)"
            ))
            .with_stable_code(StableErrorCode::AuthPermissionDenied));
        }
        409 => {
            return Err(CliError::fatal(format!(
                "mega2 server refused the {operation} request (HTTP 409)"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked));
        }
        400 => {
            return Err(CliError::fatal(format!(
                "mega2 server rejected the {operation} request (HTTP 400) — the name may not exist or already exists"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
        status if (300..400).contains(&status) => {
            return Err(CliError::fatal(format!(
                "mega2 server redirected the {operation} request (HTTP {status}) — redirects are refused"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        status if !(200..300).contains(&status) => {
            return Err(CliError::fatal(format!(
                "mega2 server returned HTTP {status} for {operation}"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        _ => {}
    }

    let envelope: WireCommonResult<T> = serde_json::from_slice(raw).map_err(|_| {
        CliError::fatal(format!(
            "mega2 server returned an invalid {operation} response"
        ))
        .with_stable_code(StableErrorCode::NetworkProtocol)
    })?;
    if !envelope.req_result {
        return Err(
            CliError::fatal(format!("mega2 {operation} failed (req_result=false)"))
                .with_stable_code(StableErrorCode::NetworkProtocol),
        );
    }
    envelope.data.ok_or_else(|| {
        CliError::fatal(format!("mega2 {operation} response carried no data"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    })
}

/// A successful mutation must name the commit it created.
fn require_commit_id(commit_id: &str, operation: &str) -> CliResult<()> {
    if commit_id.is_empty() {
        return Err(
            CliError::fatal(format!("mega2 {operation} response is missing commit_id"))
                .with_stable_code(StableErrorCode::NetworkProtocol),
        );
    }
    Ok(())
}

/// One-shot directory delete/move client.
#[derive(Debug)]
pub struct Mega2MutateClient {
    base: Url,
    http: reqwest::Client,
    token: Option<Mega2Token>,
    pin: MountedPin,
}

impl Mega2MutateClient {
    /// Builds the client with the verified pin and the MB-01 timeout.
    pub fn new(server_url: &str, token: Option<Mega2Token>) -> CliResult<Self> {
        Self::with_pin(
            server_url,
            token,
            MountedPin::verified_a1293686(),
            REQUEST_TIMEOUT,
        )
    }

    /// Test seam: construction with caller-provided pin evidence + deadline.
    /// An unverified pin refuses to start (AC-1).
    pub fn with_pin(
        server_url: &str,
        token: Option<Mega2Token>,
        pin: MountedPin,
        timeout: Duration,
    ) -> CliResult<Self> {
        if !pin.is_verified() {
            return Err(CliError::fatal(format!(
                "mega2 mutate: DEP-MB-04 pin '{}' is not verified; refusing to start",
                pin.reference
            ))
            .with_stable_code(StableErrorCode::Unsupported));
        }
        let base = validate_server_url(server_url)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|_| {
                CliError::fatal("cannot build the mega2 HTTP client")
                    .with_stable_code(StableErrorCode::InternalInvariant)
            })?;
        Ok(Self {
            base,
            http,
            token,
            pin,
        })
    }

    /// The verified pin this client started with.
    pub fn pin(&self) -> MountedPin {
        self.pin
    }

    /// Whether this client will attach an `Authorization` header.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    async fn post_json<B, T>(&self, route: &str, body: &B, operation: &str) -> CliResult<T>
    where
        B: Serialize + Sync,
        T: DeserializeOwned,
    {
        let url = self.base.join(route).map_err(|_| {
            CliError::fatal(format!("cannot build the mega2 {operation} URL"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        let mut request = self.http.post(url).json(body);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token.expose()));
        }
        let response = mega2_diag::send(request, transport_error).await?;
        let status = response.status();
        let raw = read_bounded(response).await?;
        parse_common_envelope(status.as_u16(), &raw, operation)
    }

    /// Deletes one directory child; exactly one request.
    ///
    /// `known_type` is the listing type the caller already has, if any: a known
    /// file target is refused locally, and a server type mismatch fails closed
    /// through the shared error mapping.
    pub async fn delete_directory(
        &self,
        parent_path: &str,
        name: &str,
        known_type: Option<ContentType>,
    ) -> CliResult<DeleteReceipt> {
        mega2_diag::run(
            mega2_diag::DELETE_ENTRY,
            self.delete_directory_scoped(parent_path, name, known_type),
        )
        .await
    }

    /// Body of [`Self::delete_directory`]; runs inside its diagnostics scope.
    async fn delete_directory_scoped(
        &self,
        parent_path: &str,
        name: &str,
        known_type: Option<ContentType>,
    ) -> CliResult<DeleteReceipt> {
        let parent = validate_delete_target(parent_path, name, known_type)?;
        let data: WireDeleteResult = self
            .post_json(
                DELETE_ENTRY_ROUTE,
                &delete_body(&parent, name),
                "delete-entry",
            )
            .await?;
        require_commit_id(&data.commit_id, "delete-entry")?;
        Ok(DeleteReceipt {
            commit_id: data.commit_id,
            path: data.path,
            cl_link: data.cl_link,
        })
    }

    /// Moves/renames one directory child; exactly one request. Destination
    /// collisions are refused by the server (HTTP 400) and never overwrite.
    pub async fn move_entry(
        &self,
        from_parent: &str,
        from_name: &str,
        to_parent: &str,
        to_name: &str,
        known_type: Option<ContentType>,
    ) -> CliResult<MoveReceipt> {
        mega2_diag::run(
            mega2_diag::MOVE_ENTRY,
            self.move_entry_scoped(from_parent, from_name, to_parent, to_name, known_type),
        )
        .await
    }

    /// Body of [`Self::move_entry`]; runs inside its diagnostics scope.
    async fn move_entry_scoped(
        &self,
        from_parent: &str,
        from_name: &str,
        to_parent: &str,
        to_name: &str,
        known_type: Option<ContentType>,
    ) -> CliResult<MoveReceipt> {
        let (from, to) = validate_move_target(
            from_parent,
            from_name,
            to_parent,
            to_name,
            known_type,
            false,
        )?;
        let data: WireMoveResult = self
            .post_json(
                MOVE_ENTRY_ROUTE,
                &move_body(&from, from_name, &to, to_name),
                "move-entry",
            )
            .await?;
        require_commit_id(&data.commit_id, "move-entry")?;
        Ok(MoveReceipt {
            commit_id: data.commit_id,
            from_path: data.from_path,
            to_path: data.to_path,
            cl_link: data.cl_link,
        })
    }

    /// Rename: the same client operation as move with an unchanged parent.
    pub async fn rename_directory(
        &self,
        parent_path: &str,
        from_name: &str,
        to_name: &str,
        known_type: Option<ContentType>,
    ) -> CliResult<MoveReceipt> {
        self.move_entry(parent_path, from_name, parent_path, to_name, known_type)
            .await
    }
}

fn transport_error(error: reqwest::Error) -> CliError {
    if error.is_timeout() {
        CliError::fatal("mega2 mutate request timed out")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else if error.is_connect() {
        CliError::fatal("cannot connect to the mega2 server")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else {
        CliError::fatal("mega2 mutate request failed")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    }
}

/// Streams the body with an immediate abort past [`MAX_RESPONSE_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> CliResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(CliError::fatal(format!(
                "mega2 mutate response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    struct MockMutateServer {
        addr: SocketAddr,
        requests: Arc<AtomicUsize>,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl MockMutateServer {
        /// Responds 403 when the request body's `to_path` matches `block`, 200 otherwise.
        fn start(block: Option<&'static str>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let addr = listener.local_addr().expect("addr");
            let requests = Arc::new(AtomicUsize::new(0));
            let bodies = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_clone = Arc::clone(&requests);
            let bodies_clone = Arc::clone(&bodies);
            let stop_clone = Arc::clone(&stop);
            let join = thread::spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut buf = vec![0u8; 32 * 1024];
                            let n = stream.read(&mut buf).unwrap_or(0);
                            let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                            let body = raw
                                .split_once("\r\n\r\n")
                                .map(|(_, body)| body.to_string())
                                .unwrap_or_default();
                            let json: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                            bodies_clone.lock().expect("lock").push(json.clone());
                            requests_clone.fetch_add(1, Ordering::SeqCst);
                            let blocked = block
                                .map(|needle| {
                                    json.get("to_path").and_then(|v| v.as_str()) == Some(needle)
                                })
                                .unwrap_or(false);
                            let (status, payload) = if blocked {
                                (403, serde_json::json!({"req_result": false, "data": null}))
                            } else {
                                (
                                    200,
                                    serde_json::json!({
                                        "req_result": true,
                                        "data": {
                                            "commit_id": "commit-1",
                                            "path": "/gone",
                                            "from_path": "/src/a",
                                            "to_path": "/dst/a",
                                            "cl_link": null,
                                        },
                                    }),
                                )
                            };
                            let payload = payload.to_string();
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
                bodies,
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

        fn bodies(&self) -> Vec<serde_json::Value> {
            self.bodies.lock().expect("lock").clone()
        }
    }

    impl Drop for MockMutateServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn token() -> Mega2Token {
        Mega2Token::new("test-token").expect("token")
    }

    fn snapshot(dir: &Path) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = Vec::new();
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push((path.display().to_string(), size));
        }
        entries.sort();
        entries
    }

    #[test]
    fn start_refused_without_mounted_pin() {
        let unverified = MountedPin {
            reference: "mega2@unknown",
            delete_route_mounted: false,
            move_route_mounted: false,
            push_auth_same_kind_as_create_entry: false,
        };
        let err = Mega2MutateClient::with_pin(
            "https://mega2.example.com",
            None,
            unverified,
            Duration::from_secs(1),
        )
        .expect_err("unverified pin refuses to start");
        assert_eq!(err.stable_code(), StableErrorCode::Unsupported);
        assert!(err.render().contains("DEP-MB-04"), "{}", err.render());

        // A single missing fact is enough to refuse.
        let mut partially = MountedPin::verified_a1293686();
        partially.push_auth_same_kind_as_create_entry = false;
        assert!(
            Mega2MutateClient::with_pin(
                "https://mega2.example.com",
                None,
                partially,
                Duration::from_secs(1),
            )
            .is_err()
        );

        assert!(MountedPin::verified_a1293686().is_verified());
    }

    #[test]
    fn delete_posts_path_and_fields() {
        assert_eq!(DELETE_ENTRY_ROUTE, "/api/v1/delete-entry");
        let body = serde_json::to_value(delete_body("/src", "old")).expect("serialize");
        assert_eq!(
            body,
            serde_json::json!({"path": "/src", "name": "old", "skip_build": true})
        );
        // `is_directory` and `author_username` are omitted (server default).
        assert!(body.get("is_directory").is_none());
        assert!(body.get("author_username").is_none());
        assert!(body.get("new_oid").is_none());
    }

    #[test]
    fn move_posts_path_and_fields() {
        assert_eq!(MOVE_ENTRY_ROUTE, "/api/v1/move-entry");
        let body = serde_json::to_value(move_body("/src", "a", "/dst", "b")).expect("serialize");
        assert_eq!(
            body,
            serde_json::json!({
                "from_path": "/src",
                "from_name": "a",
                "to_path": "/dst",
                "to_name": "b",
                "skip_build": true,
            })
        );
        assert!(body.get("is_directory").is_none());
        assert!(body.get("author_username").is_none());
    }

    #[test]
    fn rename_is_same_parent_move() {
        let rename = serde_json::to_value(move_body("/src", "old", "/src", "new")).expect("ser");
        assert_eq!(rename["from_path"], rename["to_path"]);
        assert_ne!(rename["from_name"], rename["to_name"]);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let server = MockMutateServer::start(None);
        let client = Mega2MutateClient::new(&server.url(), None).expect("client");
        let receipt = runtime
            .block_on(client.rename_directory("/src", "old", "new", None))
            .expect("rename succeeds");
        assert_eq!(receipt.commit_id, "commit-1");
        assert_eq!(server.requests(), 1);
        let body = server.bodies().pop().expect("body");
        assert_eq!(body["from_path"], body["to_path"]);
        assert_eq!(body["from_name"], "old");
        assert_eq!(body["to_name"], "new");
    }

    #[test]
    fn move_one_of_two_paths_403() {
        let server = MockMutateServer::start(Some("/blocked"));
        let client = Mega2MutateClient::new(&server.url(), Some(token())).expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        // The allowed destination passes the two-path gate.
        runtime
            .block_on(client.move_entry("/src", "a", "/dst", "a", None))
            .expect("allowed move");
        assert_eq!(server.requests(), 1);

        // A 403 on the second path rejects the whole request before any work.
        let err = runtime
            .block_on(client.move_entry("/src", "a", "/blocked", "a", None))
            .expect_err("403 on to_path");
        assert_eq!(err.stable_code(), StableErrorCode::AuthPermissionDenied);
        assert_eq!(server.requests(), 2, "one request per attempt");
        assert!(!err.render().contains("test-token"), "token leaked");
    }

    #[test]
    fn success_requires_2xx_req_result_commit_id() {
        let ok = br#"{"req_result":true,"data":{"commit_id":"c1","path":"/x","cl_link":null}}"#;
        let parsed: WireDeleteResult = parse_common_envelope(200, ok, "delete-entry").expect("2xx");
        require_commit_id(&parsed.commit_id, "delete-entry").expect("non-empty id");

        for status in [400, 401, 403, 409, 500] {
            assert!(
                parse_common_envelope::<WireDeleteResult>(status, ok, "delete-entry").is_err(),
                "status {status} must fail"
            );
        }
        let not_ok = br#"{"req_result":false,"data":{"commit_id":"c1"}}"#;
        assert!(parse_common_envelope::<WireDeleteResult>(200, not_ok, "delete-entry").is_err());
        let empty_id = br#"{"req_result":true,"data":{"commit_id":""}}"#;
        let parsed: WireDeleteResult =
            parse_common_envelope(200, empty_id, "delete-entry").expect("parses");
        assert!(require_commit_id(&parsed.commit_id, "delete-entry").is_err());
    }

    #[test]
    fn success_does_not_require_new_oid() {
        // A delete/move result has no `new_oid` field at all.
        let raw = br#"{"req_result":true,"data":{"commit_id":"c9","path":"/gone","cl_link":null}}"#;
        let parsed: WireDeleteResult =
            parse_common_envelope(200, raw, "delete-entry").expect("delete without new_oid");
        assert_eq!(parsed.commit_id, "c9");

        let raw =
            br#"{"req_result":true,"data":{"commit_id":"c9","from_path":"/a","to_path":"/b"}}"#;
        let parsed: WireMoveResult =
            parse_common_envelope(200, raw, "move-entry").expect("move without new_oid");
        assert_eq!(parsed.commit_id, "c9");
    }

    #[test]
    fn returned_paths_are_receipt_only() {
        // Even a hostile server path stays inert data: request bodies are built
        // exclusively from caller-validated inputs.
        let raw =
            br#"{"req_result":true,"data":{"commit_id":"c1","path":"/../../etc","cl_link":null}}"#;
        let parsed: WireDeleteResult =
            parse_common_envelope(200, raw, "delete-entry").expect("parses");
        let receipt = DeleteReceipt {
            commit_id: parsed.commit_id,
            path: parsed.path,
            cl_link: parsed.cl_link,
        };
        assert_eq!(receipt.path.as_deref(), Some("/../../etc"));

        let body = serde_json::to_value(delete_body("/safe", "name")).expect("serialize");
        assert_eq!(body["path"], "/safe");
        assert_ne!(body["path"], receipt.path.clone().unwrap_or_default());

        let move_receipt_raw =
            br#"{"req_result":true,"data":{"commit_id":"c1","from_path":"/../x","to_path":"/../y"}}"#;
        let parsed: WireMoveResult =
            parse_common_envelope(200, move_receipt_raw, "move-entry").expect("parses");
        let move_body_value =
            serde_json::to_value(move_body("/safe", "a", "/safe", "b")).expect("serialize");
        assert_eq!(move_body_value["from_path"], "/safe");
        assert_ne!(
            move_body_value["to_path"],
            parsed.to_path.unwrap_or_default()
        );
    }

    #[test]
    fn root_and_file_rejected_locally() {
        // Root deletion: an entry name is mandatory and cannot be `/` or empty.
        assert!(validate_delete_target("/", "/", None).is_err());
        assert!(validate_delete_target("/", "", None).is_err());
        assert!(validate_delete_target("/", "..", None).is_err());
        assert!(validate_delete_target("relative", "name", None).is_err());
        // File-typed targets are refused before any network call.
        let err = validate_delete_target("/", "readme.txt", Some(ContentType::File))
            .expect_err("file delete refused");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
        assert!(
            validate_move_target("/a", "f", "/b", "f", Some(ContentType::File), false).is_err()
        );
        // Destination traversal and overwrite are refused.
        assert!(validate_move_target("/a", "x", "/../escape", "x", None, false).is_err());
        assert!(validate_move_target("/a", "x", "/b", "..", None, false).is_err());
        let err =
            validate_move_target("/a", "x", "/b", "y", None, true).expect_err("overwrite refused");
        assert_eq!(err.stable_code(), StableErrorCode::Unsupported);
        // Valid inputs normalize to rooted canonical paths.
        assert_eq!(
            validate_move_target("//a//b/", "x", "/c", "y", None, false).expect("valid"),
            ("/a/b".to_string(), "/c".to_string())
        );
    }

    #[test]
    fn no_local_repo_write_or_recurse() {
        let server = MockMutateServer::start(None);
        let client = Mega2MutateClient::new(&server.url(), None).expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        // A repository-shaped directory must stay byte-identical.
        let repo_like = tempfile::tempdir().expect("tempdir");
        fs::create_dir(repo_like.path().join(".libra")).expect("mkdir .libra");
        fs::write(repo_like.path().join(".libra/libra.db"), b"sentinel").expect("db");
        let before = snapshot(repo_like.path());

        runtime
            .block_on(client.delete_directory("/", "doomed", Some(ContentType::Directory)))
            .expect("delete succeeds");

        assert_eq!(server.requests(), 1, "no recursion, no child enumeration");
        assert_eq!(snapshot(repo_like.path()), before, "no local writes");
        assert_eq!(server.bodies().len(), 1);
        let body = &server.bodies()[0];
        assert_eq!(body["name"], "doomed");
        assert_eq!(body["skip_build"], true);
    }
}
