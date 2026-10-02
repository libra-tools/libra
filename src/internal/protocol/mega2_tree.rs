//! Bounded, fail-closed client for the mega2 storage-only `GET /api/v1/tree` listing.
//!
//! plan-20260912 MB-01: one level per navigation; only `data.tree_items` is consumed;
//! server-provided `path` is a receipt, never navigation authority. The client sends
//! no Authorization header, refuses redirects/proxies, enforces finite connect/request/
//! body limits and returns a deterministic, validated listing.

use std::{
    collections::{BTreeSet, VecDeque},
    time::Duration,
};

use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// Fixed per-request deadline (connect and total request). MB-01 Performance budget.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum accepted response body for one listing. MB-01 Performance budget: 1 MiB.
pub const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// Maximum accepted entries in one listing. MB-01 Performance budget: 2,000.
pub const MAX_ENTRIES: usize = 2_000;

/// Maximum accepted item name length. MB-01 Performance budget: 4 KiB.
pub const MAX_NAME_BYTES: usize = 4 * 1024;

/// Session cache entry bound. MB-01 Performance budget: 64 listings.
pub const MAX_CACHE_ENTRIES: usize = 64;

/// Session cache byte bound. MB-01 Performance budget: 4 MiB.
pub const MAX_CACHE_BYTES: usize = 4 << 20;

/// Server route consumed by this client.
pub const TREE_ROUTE: &str = "/api/v1/tree";

/// Entry kind accepted from the wire. Anything else is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Directory,
    File,
}

/// One validated listing entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingEntry {
    pub name: String,
    pub content_type: ContentType,
}

/// A validated listing: deterministic ordering (directories first, then name ascending).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<ListingEntry>,
}

impl Listing {
    /// Deterministic cache-accounting size (sum of name bytes + one per entry).
    fn size_bytes(&self) -> usize {
        self.entries.iter().map(|e| e.name.len() + 1).sum()
    }
}

// ---- wire DTOs (only the envelope we consume; extra fields are ignored) ----

#[derive(Debug, Deserialize)]
struct WireCommonResult<T> {
    req_result: bool,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct WireTreeResponse {
    tree_items: Vec<WireTreeItem>,
}

#[derive(Debug, Deserialize)]
struct WireTreeItem {
    name: String,
    content_type: String,
}

/// Validates the user-supplied server base URL:
/// https, or http with a loopback host; no userinfo, query or fragment.
pub fn validate_server_url(raw: &str) -> CliResult<Url> {
    // Never echo `raw`: an unparsable value can still carry credentials
    // (`https://user:secret@host:badport`), and the credential check below only
    // runs on a parsed URL. `url::ParseError` renders a fixed reason without input.
    let url = Url::parse(raw).map_err(|err| {
        CliError::fatal(format!("invalid mega2 server URL: {err}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;

    let loopback_http = match url.scheme() {
        "https" => false,
        "http" => true,
        _ => {
            return Err(
                CliError::fatal("mega2 server URL must use https:// or loopback http://")
                    .with_stable_code(StableErrorCode::CliInvalidTarget),
            );
        }
    };
    if loopback_http && !is_loopback_host(url.host_str()) {
        return Err(
            CliError::fatal("mega2 server URL over http must be a loopback address")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            CliError::fatal("mega2 server URL must not contain credentials")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if url.query().is_some() {
        return Err(
            CliError::fatal("mega2 server URL must not contain a query string")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if url.fragment().is_some() {
        return Err(
            CliError::fatal("mega2 server URL must not contain a fragment")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    // The base URL must be the origin only; a subpath would be ambiguous once the
    // fixed `/api/v1/tree` route is appended.
    if !matches!(url.path(), "" | "/") {
        return Err(CliError::fatal(
            "mega2 server URL must not contain a path; the client appends /api/v1/tree",
        )
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok(url)
}

fn is_loopback_host(host: Option<&str>) -> bool {
    match host {
        Some("localhost") => true,
        Some(host) => {
            // `Url::host_str()` keeps IPv6 brackets, e.g. "[::1]".
            let host = host
                .strip_prefix('[')
                .unwrap_or(host)
                .strip_suffix(']')
                .unwrap_or(host);
            match host.parse::<std::net::IpAddr>() {
                Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback(),
                Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
                Err(_) => false,
            }
        }
        None => false,
    }
}

/// Normalizes a user-supplied rooted path to its canonical form.
///
/// Accepts `/` or `/a/b`; collapses repeated separators; refuses relative paths,
/// `.` / `..` components, NUL, control characters and `\` (platform-separator
/// ambiguity — the canonical separator is always `/`).
pub fn normalize_path(input: &str) -> CliResult<String> {
    if !input.starts_with('/') {
        return Err(
            CliError::fatal("mega2 path must be rooted, starting with '/'")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if input.contains('\\') {
        return Err(
            CliError::fatal("mega2 path must use '/' separators (found '\\\\')")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if input.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(
            CliError::fatal("mega2 path contains NUL or control characters")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }

    let mut segments = Vec::new();
    for segment in input.split('/') {
        if segment.is_empty() {
            continue;
        }
        match segment {
            "." | ".." => {
                return Err(CliError::fatal(format!(
                    "mega2 path must not contain '{segment}' components"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget));
            }
            _ => segments.push(segment),
        }
    }

    let mut normalized = String::from("/");
    normalized.push_str(&segments.join("/"));
    Ok(normalized)
}

/// Conservative item-name validation: non-empty, bounded, no NUL/control,
/// no separators, not `.` / `..`.
fn validate_item_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    !name
        .chars()
        .any(|c| c == '\0' || c == '/' || c == '\\' || c.is_control())
}

/// Bounded LRU cache for validated listings, with entry and byte accounting.
#[derive(Debug, Default)]
pub struct ListingCache {
    entries: VecDeque<(String, Option<String>, Listing)>,
    bytes: usize,
}

impl ListingCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks a listing up without network; moves the hit to the back (LRU touch).
    pub fn get(&mut self, path: &str, git_ref: Option<&str>) -> Option<Listing> {
        let key = (path.to_string(), git_ref.map(str::to_string));
        if let Some(index) = self
            .entries
            .iter()
            .position(|(p, r, _)| (p.as_str(), r.as_deref()) == (key.0.as_str(), key.1.as_deref()))
        {
            // INVARIANT: `index` came from `position()` over the same deque.
            let (_, _, listing) = self.entries.remove(index).expect("index is in range");
            let cloned = listing.clone();
            self.entries.push_back((key.0, key.1, listing));
            Some(cloned)
        } else {
            None
        }
    }

    /// Inserts a listing, evicting the oldest entries until both bounds hold.
    /// A single listing larger than [`MAX_CACHE_BYTES`] is not cached.
    pub fn insert(&mut self, path: &str, git_ref: Option<&str>, listing: Listing) {
        let size = listing.size_bytes();
        if size > MAX_CACHE_BYTES {
            return;
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_CACHE_ENTRIES || self.bytes + size > MAX_CACHE_BYTES)
        {
            // INVARIANT: the loop condition guarantees the deque is non-empty.
            let (_, _, evicted) = self.entries.pop_front().expect("non-empty checked");
            self.bytes = self.bytes.saturating_sub(evicted.size_bytes());
        }
        self.bytes += size;
        self.entries
            .push_back((path.to_string(), git_ref.map(str::to_string), listing));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn byte_count(&self) -> usize {
        self.bytes
    }
}

/// A bounded HTTP client for the mega2 tree route.
#[derive(Debug, Clone)]
pub struct Mega2TreeClient {
    base: Url,
    http: reqwest::Client,
}

impl Mega2TreeClient {
    /// Builds the client with the default [`REQUEST_TIMEOUT`].
    pub fn new(server_url: &str) -> CliResult<Self> {
        Self::with_timeouts(server_url, REQUEST_TIMEOUT)
    }

    /// Test seam: same builder with a caller-provided deadline.
    pub fn with_timeouts(server_url: &str, timeout: Duration) -> CliResult<Self> {
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
        Ok(Self { base, http })
    }

    /// One GET per listing: `{base}/api/v1/tree?path=<normalized>[&refs=<ref>]`.
    /// No Authorization header; response body bounded by [`MAX_RESPONSE_BYTES`].
    pub async fn fetch_listing(&self, path: &str, git_ref: Option<&str>) -> CliResult<Listing> {
        let normalized = normalize_path(path)?;
        let mut url = self.base.join(TREE_ROUTE).map_err(|_| {
            CliError::fatal("cannot build the mega2 tree URL")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        url.query_pairs_mut().append_pair("path", &normalized);
        if let Some(git_ref) = git_ref
            && !git_ref.is_empty()
        {
            url.query_pairs_mut().append_pair("refs", git_ref);
        }

        let response = self.http.get(url).send().await.map_err(transport_error)?;
        let status = response.status();
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(CliError::fatal(format!(
                    "mega2 server refused the listing (HTTP {status})"
                ))
                .with_stable_code(StableErrorCode::AuthPermissionDenied));
            }
            status if status.is_redirection() => {
                return Err(CliError::fatal(format!(
                    "mega2 server redirected (HTTP {status}) — redirects are refused"
                ))
                .with_stable_code(StableErrorCode::NetworkProtocol));
            }
            status if !status.is_success() => {
                return Err(
                    CliError::fatal(format!("mega2 server returned HTTP {status}"))
                        .with_stable_code(StableErrorCode::NetworkProtocol),
                );
            }
            _ => {}
        }

        let body = read_bounded(response).await?;
        let envelope: WireCommonResult<WireTreeResponse> =
            serde_json::from_slice(&body).map_err(|_| {
                CliError::fatal("mega2 server returned an invalid tree response")
                    .with_stable_code(StableErrorCode::NetworkProtocol)
            })?;
        if !envelope.req_result {
            return Err(
                CliError::fatal("mega2 tree request failed (req_result=false)")
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            );
        }
        let data = envelope.data.ok_or_else(|| {
            CliError::fatal("mega2 tree response carried no data")
                .with_stable_code(StableErrorCode::NetworkProtocol)
        })?;
        validate_and_sort(data.tree_items)
    }
}

/// Maps transport failures to a stable, URL-free error.
fn transport_error(error: reqwest::Error) -> CliError {
    if error.is_timeout() {
        CliError::fatal("mega2 listing request timed out")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else if error.is_connect() {
        CliError::fatal("cannot connect to the mega2 server")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else {
        CliError::fatal("mega2 listing request failed")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    }
}

/// Streams the body with an immediate abort past [`MAX_RESPONSE_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> CliResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(CliError::fatal(format!(
                "mega2 tree response exceeds the {}-byte limit",
                MAX_RESPONSE_BYTES
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Validates wire items and returns the deterministic listing.
/// `file_tree` and remote item `path` are ignored by construction (not deserialized).
fn validate_and_sort(items: Vec<WireTreeItem>) -> CliResult<Listing> {
    if items.len() > MAX_ENTRIES {
        return Err(CliError::fatal(format!(
            "mega2 tree response has more than {MAX_ENTRIES} entries"
        ))
        .with_stable_code(StableErrorCode::NetworkProtocol));
    }

    let mut seen = BTreeSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        if !validate_item_name(&item.name) {
            return Err(
                CliError::fatal("mega2 tree response contained an untrusted item name")
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            );
        }
        if !seen.insert(item.name.clone()) {
            return Err(
                CliError::fatal("mega2 tree response contained duplicate item names")
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            );
        }
        let content_type = match item.content_type.as_str() {
            "directory" => ContentType::Directory,
            "file" => ContentType::File,
            _ => {
                return Err(CliError::fatal(
                    "mega2 tree response contained an unsupported content_type",
                )
                .with_stable_code(StableErrorCode::NetworkProtocol));
            }
        };
        entries.push(ListingEntry {
            name: item.name,
            content_type,
        });
    }

    // Deterministic: directories first, then name ascending.
    entries.sort_by(|a, b| {
        let a_is_dir = a.content_type == ContentType::Directory;
        let b_is_dir = b.content_type == ContentType::Directory;
        b_is_dir.cmp(&a_is_dir).then_with(|| a.name.cmp(&b.name))
    });
    Ok(Listing { entries })
}

/// Session wrapper: client + bounded cache + request accounting.
/// Always issues exactly one request per `fetch` call — no prefetch, no background work.
#[derive(Debug)]
pub struct Mega2TreeSession {
    client: Mega2TreeClient,
    cache: ListingCache,
    request_count: u64,
}

impl Mega2TreeSession {
    pub fn new(server_url: &str) -> CliResult<Self> {
        Ok(Self {
            client: Mega2TreeClient::new(server_url)?,
            cache: ListingCache::new(),
            request_count: 0,
        })
    }

    pub fn with_client(client: Mega2TreeClient) -> Self {
        Self {
            client,
            cache: ListingCache::new(),
            request_count: 0,
        }
    }

    /// Fetches one listing (exactly one request) and stores it in the bounded cache.
    pub async fn fetch(&mut self, path: &str, git_ref: Option<&str>) -> CliResult<Listing> {
        let listing = self.client.fetch_listing(path, git_ref).await?;
        self.request_count += 1;
        self.cache.insert(path, git_ref, listing.clone());
        Ok(listing)
    }

    /// Cache lookup without network.
    pub fn cached(&mut self, path: &str, git_ref: Option<&str>) -> Option<Listing> {
        self.cache.get(path, git_ref)
    }

    pub fn request_count(&self) -> u64 {
        self.request_count
    }

    pub fn cache_entry_count(&self) -> usize {
        self.cache.entry_count()
    }

    pub fn cache_byte_count(&self) -> usize {
        self.cache.byte_count()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    use super::*;

    /// Minimal blocking mock HTTP server: records each request head verbatim and
    /// replies with one canned response (or stalls when `stall` is set).
    struct MockTreeServer {
        addr: std::net::SocketAddr,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
    }

    impl MockTreeServer {
        fn start(canned: String, stall: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock tree server");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("mock tree addr");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_clone = Arc::clone(&requests);
            let stop_clone = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_nonblocking(false)
                                .expect("blocking mock connection");
                            let mut buf = vec![0u8; 64 * 1024];
                            let mut head = Vec::new();
                            let mut pending = String::new();
                            loop {
                                let n = stream.read(&mut buf).expect("mock read");
                                if n == 0 {
                                    break;
                                }
                                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                                if let Some(end) = pending.find("\r\n\r\n") {
                                    head.extend_from_slice(&pending.as_bytes()[..=end]);
                                    break;
                                }
                            }
                            requests_clone
                                .lock()
                                .expect("requests poisoned")
                                .push(String::from_utf8_lossy(&head).into_owned());
                            if !stall {
                                if let Err(error) = stream.write_all(canned.as_bytes()) {
                                    // An oversized response is intentionally abandoned by the client.
                                    let expected_disconnect = canned.len() > MAX_RESPONSE_BYTES
                                        && matches!(
                                            error.kind(),
                                            std::io::ErrorKind::BrokenPipe
                                                | std::io::ErrorKind::ConnectionReset
                                                | std::io::ErrorKind::ConnectionAborted
                                        );
                                    assert!(expected_disconnect, "mock write: {error}");
                                }
                            } else {
                                // Hold the connection open without a response to
                                // exercise the client-side total timeout.
                                thread::sleep(Duration::from_secs(5));
                            }
                            let _ = stream.flush();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("mock accept failed: {e}"),
                    }
                }
            });
            Self {
                addr,
                requests,
                stop,
            }
        }

        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("requests poisoned").clone()
        }
    }

    impl Drop for MockTreeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn status_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn envelope(tree_items: &str, extra: &str) -> String {
        // Plain (non-format) raw string so the nested JSON braces stay literal.
        let file_tree = r#"{"/":{"total_count":3,"tree_items":[{"name":"junk","path":"/x","content_type":"file"}]}}"#;
        format!(
            r#"{{"req_result":true,"data":{{"tree_items":[{tree_items}],"file_tree":{file_tree}{extra}}},"err_message":""}}"#
        )
    }

    fn item(name: &str, content_type: &str) -> String {
        format!(r#"{{"name":"{name}","path":"/","content_type":"{content_type}"}}"#)
    }

    // ---- URL validation (AC-1 / AC-2) ----

    #[test]
    fn url_accepts_https_and_loopback_http_only() {
        assert!(validate_server_url("https://example.com").is_ok());
        assert!(validate_server_url("https://example.com:8443").is_ok());
        assert!(validate_server_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_server_url("http://localhost:8080").is_ok());
        assert!(validate_server_url("http://[::1]:8080").is_ok());
    }

    #[test]
    fn url_rejects_insecure_or_ambiguous_forms() {
        for bad in [
            "http://example.com",
            "http://192.168.1.10",
            "ftp://example.com",
            "https://user:pass@example.com",
            "https://example.com/path",
            "https://example.com?x=1",
            "https://example.com#frag",
            "not a url",
        ] {
            let err = validate_server_url(bad).expect_err(bad);
            assert_eq!(
                err.stable_code(),
                StableErrorCode::CliInvalidTarget,
                "{bad}"
            );
        }
    }

    // plan-20261001 MN-10: an unparsable URL is rejected before the credential
    // check, so its error must not carry any part of the raw input.
    #[test]
    fn url_parse_error_does_not_echo_input() {
        let raw = "https://user:MARKER@mega2.example.com:notaport";
        let err = validate_server_url(raw).expect_err("unparsable port");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
        assert!(
            !err.message().contains("MARKER"),
            "message echoes input: {}",
            err.message()
        );
        let json = err.render_json();
        assert!(
            !json.contains("MARKER"),
            "JSON envelope echoes input: {json}"
        );
    }

    // ---- path normalization (AC-3) ----

    #[test]
    fn path_normalizer_accepts_rooted_separator_normalized_paths() {
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("/a").unwrap(), "/a");
        assert_eq!(normalize_path("/a/b").unwrap(), "/a/b");
        assert_eq!(normalize_path("/a//b/").unwrap(), "/a/b");
        assert_eq!(normalize_path("/src sub").unwrap(), "/src sub");
    }

    #[test]
    fn path_normalizer_refuses_traversal_nul_and_separator_ambiguity() {
        for bad in [
            "",
            "src",
            "/a/../b",
            "/./a",
            "/..",
            "/a\\b",
            "/a\0b",
            "/a\nb",
            "/a\u{1b}[31m",
        ] {
            let err = normalize_path(bad).expect_err(bad);
            assert_eq!(
                err.stable_code(),
                StableErrorCode::CliInvalidTarget,
                "{bad:?}"
            );
        }
    }

    // ---- fetch behaviour (AC-1 / AC-4 / AC-6 / AC-7) ----

    #[tokio::test]
    async fn fetch_sends_one_encoded_request_without_auth_and_ignores_file_tree() {
        let server = MockTreeServer::start(
            ok_response(&envelope(
                &format!("{},{}", item("b-file", "file"), item("a-dir", "directory")),
                r#","unknown_field":{"x":1}"#,
            )),
            false,
        );
        let mut session = Mega2TreeSession::new(&server.url()).unwrap();
        let listing = session.fetch("/src sub", Some("abc def")).await.unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 1, "exactly one request per listing");
        let head = &requests[0];
        assert!(head.starts_with("GET /api/v1/tree?"), "{head}");
        assert!(head.contains("path=%2Fsrc+sub"), "{head}");
        assert!(head.contains("refs=abc+def"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
        assert_eq!(session.request_count(), 1);
        assert_eq!(listing.entries.len(), 2);
    }

    #[tokio::test]
    async fn valid_listing_is_deterministic_directories_first_then_name() {
        let server = MockTreeServer::start(
            ok_response(&envelope(
                &format!(
                    "{},{},{},{}",
                    item("z-file", "file"),
                    item("b-dir", "directory"),
                    item("a-file", "file"),
                    item("a-dir", "directory")
                ),
                "",
            )),
            false,
        );
        let listing = Mega2TreeClient::new(&server.url())
            .unwrap()
            .fetch_listing("/", None)
            .await
            .unwrap();
        let names: Vec<(&str, ContentType)> = listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.content_type))
            .collect();
        assert_eq!(
            names,
            vec![
                ("a-dir", ContentType::Directory),
                ("b-dir", ContentType::Directory),
                ("a-file", ContentType::File),
                ("z-file", ContentType::File),
            ]
        );
    }

    #[tokio::test]
    async fn http_failures_map_to_stable_codes() {
        for (status, expected) in [
            ("401 Unauthorized", StableErrorCode::AuthPermissionDenied),
            ("403 Forbidden", StableErrorCode::AuthPermissionDenied),
            (
                "500 Internal Server Error",
                StableErrorCode::NetworkProtocol,
            ),
            ("302 Found", StableErrorCode::NetworkProtocol),
        ] {
            let server = MockTreeServer::start(status_response(status, "{}"), false);
            let err = Mega2TreeClient::new(&server.url())
                .unwrap()
                .fetch_listing("/", None)
                .await
                .expect_err(status);
            assert_eq!(err.stable_code(), expected, "{status}");
        }
    }

    #[tokio::test]
    async fn malformed_envelopes_fail_closed_without_body_echo() {
        let marker = "SECRETMARKER-SHOULD-NOT-LEAK";
        for (body, label) in [
            (
                format!(r#"{{"req_result":false,"data":null,"err_message":"{marker}"}}"#),
                "req_result=false",
            ),
            (
                format!(r#"{{"req_result":true,"data":null,"err_message":"{marker}"}}"#),
                "data=null",
            ),
            (format!(r#"not-json-{marker}"#), "bad json"),
            (
                format!(
                    r#"{{"req_result":true,"data":{{"tree_items":[{{"name":"ok","path":"/","content_type":"{marker}"}}]}},"err_message":""}}"#
                ),
                "unsupported content_type",
            ),
        ] {
            let server = MockTreeServer::start(ok_response(&body), false);
            let err = Mega2TreeClient::new(&server.url())
                .unwrap()
                .fetch_listing("/", None)
                .await
                .expect_err(label);
            assert_eq!(
                err.stable_code(),
                StableErrorCode::NetworkProtocol,
                "{label}"
            );
            assert!(
                !err.message().contains(marker),
                "{label}: body leaked into error: {}",
                err.message()
            );
        }
    }

    #[tokio::test]
    async fn hostile_entries_are_rejected() {
        let long_name = "x".repeat(MAX_NAME_BYTES + 1);
        for (items, label) in [
            (item("..", "directory"), "dot-dot name"),
            (item(".", "directory"), "dot name"),
            (
                r#"{"name":"a\u0000b","path":"/","content_type":"directory"}"#.to_string(),
                "NUL name",
            ),
            (
                r#"{"name":"a\u001b[31m","path":"/","content_type":"directory"}"#.to_string(),
                "control-char name",
            ),
            (
                format!("{},{}", item("dup", "directory"), item("dup", "file")),
                "duplicate name",
            ),
            (item(&long_name, "file"), "oversize name"),
        ] {
            let server = MockTreeServer::start(ok_response(&envelope(&items, "")), false);
            let err = Mega2TreeClient::new(&server.url())
                .unwrap()
                .fetch_listing("/", None)
                .await
                .expect_err(label);
            assert_eq!(
                err.stable_code(),
                StableErrorCode::NetworkProtocol,
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn entry_count_limit_is_enforced() {
        let items: Vec<String> = (0..=MAX_ENTRIES)
            .map(|i| item(&format!("n{i}"), "file"))
            .collect();
        let server = MockTreeServer::start(ok_response(&envelope(&items.join(","), "")), false);
        let err = Mega2TreeClient::new(&server.url())
            .unwrap()
            .fetch_listing("/", None)
            .await
            .expect_err("over-limit entries");
        assert_eq!(err.stable_code(), StableErrorCode::NetworkProtocol);
    }

    #[tokio::test]
    async fn oversized_body_is_aborted() {
        let pad = "A".repeat(MAX_RESPONSE_BYTES + 1);
        let body = format!(
            r#"{{"req_result":true,"data":{{"tree_items":[],"pad":"{pad}"}},"err_message":""}}"#
        );
        let server = MockTreeServer::start(ok_response(&body), false);
        let err = Mega2TreeClient::new(&server.url())
            .unwrap()
            .fetch_listing("/", None)
            .await
            .expect_err("oversized body");
        assert_eq!(err.stable_code(), StableErrorCode::NetworkProtocol);
        assert!(err.message().contains("exceeds"), "{}", err.message());
    }

    #[tokio::test]
    async fn stalled_server_maps_to_timeout() {
        let server = MockTreeServer::start(String::new(), true);
        let err = Mega2TreeClient::with_timeouts(&server.url(), Duration::from_millis(300))
            .unwrap()
            .fetch_listing("/", None)
            .await
            .expect_err("stalled server");
        assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
        assert!(err.message().contains("timed out"), "{}", err.message());
    }

    // ---- session cache accounting (AC-7) ----

    #[tokio::test]
    async fn session_cache_respects_entry_and_byte_bounds() {
        let server =
            MockTreeServer::start(ok_response(&envelope(&item("only", "file"), "")), false);
        let mut session = Mega2TreeSession::new(&server.url()).unwrap();
        for i in 0..100 {
            let path = format!("/d{i}");
            session.fetch(&path, None).await.unwrap();
        }
        assert_eq!(session.request_count(), 100);
        assert!(session.cache_entry_count() <= MAX_CACHE_ENTRIES);
        assert!(session.cache_byte_count() <= MAX_CACHE_BYTES);

        // A hit returns the cached listing without a new request.
        let before = session.request_count();
        assert!(session.cached("/d99", None).is_some());
        assert_eq!(session.request_count(), before);
    }

    #[test]
    fn cache_refuses_an_entry_larger_than_the_byte_budget() {
        let mut cache = ListingCache::new();
        let listing = Listing {
            entries: vec![ListingEntry {
                name: "x".repeat(MAX_CACHE_BYTES + 1),
                content_type: ContentType::File,
            }],
        };
        cache.insert("/", None, listing);
        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.byte_count(), 0);
    }

    #[tokio::test]
    async fn fetch_makes_no_recursive_or_prefetch_requests() {
        // The canned envelope carries ancestor data in `file_tree`; the client must
        // consume only the requested level and never issue follow-up requests.
        let server = MockTreeServer::start(
            ok_response(&envelope(&item("child", "directory"), "")),
            false,
        );
        let listing = Mega2TreeClient::new(&server.url())
            .unwrap()
            .fetch_listing("/a/b/c", None)
            .await
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(server.requests().len(), 1);
    }
}
