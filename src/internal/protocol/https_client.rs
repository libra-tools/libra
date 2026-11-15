//! HTTPS smart protocol client that discovers refs, negotiates upload-pack/receive-pack, streams pack data, and supports basic authentication.

use std::{io::Error as IoError, ops::Deref, sync::Mutex, time::Duration};

use futures_util::{StreamExt, TryStreamExt};
use git_internal::{errors::GitError, hash::HashKind};
use reqwest::{RequestBuilder, Response, StatusCode, header::CONTENT_TYPE};
use url::Url;

use super::{
    DiscoveryResult, FetchStream, ProtocolClient, ProtocolVersion,
    generate_upload_pack_content_with_capabilities, parse_discovered_references,
    v2::{self, GIT_PROTOCOL_HEADER, GIT_PROTOCOL_VERSION_2},
};
use crate::{
    command::ask_basic_auth,
    git_protocol::ServiceType,
    utils::{
        backoff::{RetryOutcome, RetryPolicy, parse_retry_after, retry_idempotent},
        error::emit_warning,
        redact::redact_url_credentials,
    },
};

/// Ordinary HTTPS transports do not carry a Mega2 traces locator.
pub(crate) fn ordinary_code_transport_allows(url: &str) -> Result<(), String> {
    super::mega2_traces::reject_traces_locator_on_code_transport(url)
}

/// A Git protocol client that communicates with a Git server over HTTPS.
/// Only support `SmartProtocol` now, see [http-protocol](https://www.git-scm.com/docs/http-protocol) for protocol details.
pub struct HttpsClient {
    pub(crate) url: Url,
    pub(crate) client: reqwest::Client,
}

/// Default connection timeout for initial TCP+TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Configuration key selecting the wire protocol version (issues/15).
///
/// `2` (the default) negotiates protocol v2 and falls back to v0/v1 when the
/// server answers with a legacy advertisement; `0` or `1` forces the legacy
/// path for troubleshooting.
pub(crate) const PROTOCOL_VERSION_CONFIG_KEY: &str = "protocol.version";

/// Default idle (read) timeout — triggers when no bytes arrive for this duration.
/// This acts as an "idle timeout" rather than a total-request timeout: as long as
/// the server keeps sending data the timer resets, but if the connection stalls for
/// longer than this the request is aborted.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

impl ProtocolClient for HttpsClient {
    fn from_url(url: &Url) -> Self {
        // INVARIANT: the default timeout constants are valid reqwest durations;
        // if construction fails, the process lacks a usable TLS/backend setup.
        Self::from_url_with_timeouts(url, CONNECT_TIMEOUT, READ_TIMEOUT)
            .expect("reqwest client builder failed (likely missing TLS backend)")
    }
}

/// simply authentication: `username` and `password`
#[derive(Debug, Clone, PartialEq)]
pub struct BasicAuth {
    pub(crate) username: String,
    pub(crate) password: String,
}
static AUTH: Mutex<Option<BasicAuth>> = Mutex::new(None);
impl BasicAuth {
    /// set username & password manually
    pub async fn set_auth(auth: BasicAuth) {
        AUTH.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .replace(auth);
    }

    /// send request with basic auth, retry 3 times
    pub async fn send<Fut>(request_builder: impl Fn() -> Fut) -> Result<Response, reqwest::Error>
    where
        Fut: std::future::Future<Output = RequestBuilder>,
    {
        const MAX_TRY: usize = 3;
        let mut res;
        let mut try_cnt = 0;
        let mut prompted = false;
        loop {
            let mut request = request_builder().await; // RequestBuilder can't be cloned
            // Poison-tolerant: the guarded data is a plain Option swap, so a
            // panicked writer cannot leave it inconsistent — never crash the
            // network hot path over a poisoned mutex.
            let interactive = AUTH
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .deref()
                .clone();
            if let Some(auth) = interactive {
                request = request.basic_auth(auth.username.clone(), Some(auth.password.clone()));
                res = request.send().await?;
            } else {
                // Stored-token attach (lore.md 1.6): split the builder so the
                // request URL is known, then attach the host-scoped token —
                // ONLY when the scope matches and no Authorization header is
                // already present. Builder errors propagate exactly as
                // .send() would have surfaced them.
                let (client, built) = request.build_split();
                let mut built = built?;
                if built
                    .headers()
                    .get(reqwest::header::AUTHORIZATION)
                    .is_none()
                    && let Some(scope) =
                        crate::internal::auth::HostScope::from_request_url(built.url())
                {
                    match crate::internal::auth::lookup(&scope).await {
                        crate::internal::auth::Lookup::Valid { username, token } => {
                            use base64::Engine;
                            let value = format!(
                                "Basic {}",
                                base64::engine::general_purpose::STANDARD
                                    .encode(format!("{username}:{token}"))
                            );
                            if let Ok(header) = reqwest::header::HeaderValue::from_str(&value) {
                                let mut header = header;
                                header.set_sensitive(true);
                                built
                                    .headers_mut()
                                    .insert(reqwest::header::AUTHORIZATION, header);
                            }
                        }
                        crate::internal::auth::Lookup::Expired { .. } => {
                            emit_warning(format!(
                                "stored token for {} is EXPIRED; run 'libra auth login --host {}'",
                                scope.display(),
                                scope.display()
                            ));
                        }
                        crate::internal::auth::Lookup::Undecryptable => {
                            emit_warning(format!(
                                "stored token for {} cannot be decrypted (key changed?); \
                                 run 'libra auth login --host {}'",
                                scope.display(),
                                scope.display()
                            ));
                        }
                        crate::internal::auth::Lookup::Miss => {}
                    }
                }
                res = client.execute(built).await?;
            }
            if res.status() == StatusCode::FORBIDDEN {
                // 403: no access, no need to retry. The caller receives the
                // response and decides how to handle it — some callers (e.g.
                // LFS Chunks API) expect 403 as a normal "not supported" signal,
                // so do not print an alarming message here.
                tracing::warn!("HTTP 403 Forbidden from server; caller will decide how to handle");
                break;
            } else if res.status() != StatusCode::UNAUTHORIZED {
                break;
            }
            // 401 (Unauthorized): username or password is incorrect
            if try_cnt >= MAX_TRY {
                eprintln!("fatal: failed to authenticate after {MAX_TRY} attempts");
                break;
            }
            // 401 auto-guidance (lore.md 2.7): a non-TTY caller must never
            // hit the interactive prompt (it would consume piped protocol
            // data) — fail fast with the auth-login hint instead.
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                let scope = crate::internal::auth::HostScope::from_request_url(res.url());
                let hint = scope
                    .map(|scope| scope.display())
                    .unwrap_or_else(|| "<host>".to_string());
                eprintln!(
                    "fatal: authentication required; run 'libra auth login --host {hint}' \
                     (or 'libra auth status --host {hint}' to inspect the stored token)"
                );
                break;
            }
            if try_cnt == 0
                && let Some(scope) = crate::internal::auth::HostScope::from_request_url(res.url())
            {
                eprintln!(
                    "tip: 'libra auth login --host {}' stores a token so you are not \
                     prompted again",
                    scope.display()
                );
            }
            emit_warning("authentication required, retrying...");
            AUTH.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .replace(ask_basic_auth());
            prompted = true;
            try_cnt += 1;
        }
        // Persist-on-success offer (lore.md 2.7): after a PROMPTED attempt
        // genuinely succeeds (2xx — a 403 may be rate-limiting with WRONG
        // credentials, never an identity proof), offer ONCE per scope per
        // process to store the credential through the auth machinery.
        // Consent-based: default No; `auth.saveOnPrompt` = ask|always|never.
        if prompted && res.status().is_success() {
            maybe_offer_persist(res.url()).await;
        }
        Ok(res)
    }
}

/// The consent flow for storing an interactively-entered credential. Never
/// silent: `always` skips the question but still prints what it stored
/// (scope only — no secret ever reaches output or logs).
async fn maybe_offer_persist(url: &url::Url) {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return;
    }
    let Some(scope) = crate::internal::auth::HostScope::from_request_url(url) else {
        return; // non-https (non-loopback) would never attach anyway
    };
    // Once per scope per process.
    static OFFERED: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    {
        let mut offered = OFFERED.lock().unwrap_or_else(|poison| poison.into_inner());
        let set = offered.get_or_insert_with(Default::default);
        if !set.insert(scope.display()) {
            return;
        }
    }
    let policy = crate::internal::config::ConfigKv::get("auth.saveOnPrompt")
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "ask".to_string());
    let store = match policy.as_str() {
        "never" => return,
        "always" => true,
        _ => {
            eprint!(
                "Store this credential for {} (encrypted at rest)? [y/N] ",
                scope.display()
            );
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                return;
            }
            matches!(answer.trim(), "y" | "Y" | "yes")
        }
    };
    if !store {
        return;
    }
    let credentials = AUTH
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let Some(credentials) = credentials else {
        return;
    };
    // The same validation the auth-login path applies (token rules PLUS the
    // username rules: non-empty, no ':', no control characters).
    if credentials.password.is_empty()
        || credentials.password.len() > 8192
        || credentials.password.chars().any(|c| c.is_control())
        || credentials.username.is_empty()
        || credentials.username.contains(':')
        || credentials.username.chars().any(|c| c.is_control())
    {
        return;
    }
    match crate::internal::auth::store_token(
        &scope,
        &credentials.username,
        &credentials.password,
        None,
    )
    .await
    {
        Ok(()) => eprintln!("Stored token for {}", scope.display()),
        Err(error) => eprintln!("warning: could not store the credential: {error}"),
    }
}

// Client communicates with the remote git repository over SMART protocol.
// protocol details: https://www.git-scm.com/docs/http-protocol
// capability declarations: https://www.git-scm.com/docs/protocol-capabilities
impl HttpsClient {
    pub fn from_url_with_timeouts(
        url: &Url,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, String> {
        let url = normalize_url(url);
        let client = build_client(connect_timeout, read_timeout, url_is_loopback(&url))?;
        Ok(Self { url, client })
    }

    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, String> {
        self.client = build_client(connect_timeout, read_timeout, url_is_loopback(&self.url))?;
        Ok(self)
    }

    /// GET $GIT_URL/info/refs?service=git-upload-pack HTTP/1.0<br>
    /// Discover the references of the remote repository before fetching the objects.
    /// the first ref named HEAD as default ref.
    /// ## Args
    /// - auth: (username, password)
    pub async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        let version = preferred_protocol_version(service).await;
        let service_name = service.to_string();
        // INVARIANT: service_name is always a static "git-upload-pack" / "git-receive-pack"
        // value and self.url is validated by from_url.
        let url = self
            .url
            .join(&format!("info/refs?service={service_name}"))
            .expect("info/refs?service=... is a valid relative URL");

        // The info/refs discovery is a pure GET, so it is safe to retry with
        // bounded backoff when the server rate-limits (`429`) or is temporarily
        // unavailable (`503`), honouring `Retry-After`. Error messages are
        // credential-redacted so a `user:token@host` URL never reaches logs.
        let policy = RetryPolicy::default();
        let client = &self.client;
        let url_ref = &url;
        let res = retry_idempotent(&policy, move |_attempt| async move {
            let send_result = BasicAuth::send(|| async {
                let mut request = client.get(url_ref.clone());
                if version.is_v2() {
                    request = request.header(GIT_PROTOCOL_HEADER, GIT_PROTOCOL_VERSION_2);
                }
                request
            })
            .await;
            let res = match send_result {
                Ok(res) => res,
                Err(err) => {
                    let message = format!(
                        "Failed to send request: {}",
                        redact_url_credentials(&err.to_string())
                    );
                    // A connection-level failure never reached the server, so a
                    // retry cannot duplicate any effect.
                    if err.is_connect() {
                        return RetryOutcome::Retry {
                            retry_after: None,
                            last_err: GitError::NetworkError(message),
                        };
                    }
                    return RetryOutcome::Done(Err(GitError::NetworkError(message)));
                }
            };
            if matches!(res.status().as_u16(), 429 | 503) {
                let retry_after = res
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after);
                return RetryOutcome::Retry {
                    retry_after,
                    last_err: GitError::NetworkError(format!(
                        "server rate-limited or unavailable (HTTP {})",
                        res.status().as_u16()
                    )),
                };
            }
            RetryOutcome::Done(Ok(res))
        })
        .await?;
        // Do NOT log the `Response` via `Debug`: it embeds the request URL,
        // which can carry `user:token@host` credentials. Log status + a
        // credential-redacted URL instead.
        tracing::debug!(
            "discovery response: status={}, url={}",
            res.status(),
            redact_url_credentials(res.url().as_str())
        );

        if res.status() == 401 {
            return Err(GitError::UnAuthorized(
                "May need to provide username and password".to_string(),
            ));
        }
        // check status code MUST be 200 or 304
        if res.status() != 200 && res.status() != 304 {
            return Err(GitError::NetworkError(format!(
                "Error Response format, status code: {}",
                res.status()
            )));
        }

        // check Content-Type MUST be application/x-$servicename-advertisement
        let content_type = res
            .headers()
            .get("Content-Type")
            .ok_or_else(|| GitError::NetworkError("Missing Content-Type header".to_string()))?
            .to_str()
            .map_err(|e| GitError::NetworkError(format!("Invalid Content-Type header: {}", e)))?;
        let expected = format!("application/x-{service_name}-advertisement");
        let content_type = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim();
        if content_type != expected {
            return Err(GitError::NetworkError(format!(
                "Content-type must be `{expected}`, but got: {content_type}"
            )));
        }

        let response_content = res.bytes().await.map_err(|e| {
            GitError::NetworkError(format!(
                "Failed to read response body: {}",
                redact_url_credentials(&e.to_string())
            ))
        })?;
        // Log only the size, never the full body bytes (lore.md §0.2: do not
        // echo complete response bodies).
        tracing::debug!("discovery response body: {} bytes", response_content.len());

        if version.is_v2() && v2::is_v2_advertisement(&response_content) {
            return self.discover_v2(response_content).await;
        }
        // The server answered with a v0/v1 advertisement: keep the legacy parser
        // (and its self-reported version) untouched.
        parse_discovered_references(response_content, service)
    }

    /// Complete a protocol v2 discovery: parse the capability advertisement and
    /// ask the server for references with a `ls-refs` command.
    async fn discover_v2(&self, body: bytes::Bytes) -> Result<DiscoveryResult, GitError> {
        let advertisement = v2::parse_advertisement(body)?;
        tracing::debug!(
            "protocol v2 advertisement: {} capability lines, object-format={}",
            advertisement.capabilities.len(),
            crate::internal::object_format::as_str(advertisement.hash_kind)
        );
        let request =
            v2::ls_refs::build_request(&advertisement, &v2::ls_refs::LsRefsOptions::complete());
        let response = self.post_ls_refs(request).await?;
        let refs = v2::ls_refs::parse_response(response, advertisement.hash_kind)?;
        v2::ls_refs::validate_ref_names(&refs)?;
        // v0 advertises `symref=HEAD:<target>` alongside the refs; deriving the
        // same capability keeps default-branch resolution and `--symref` output
        // identical across protocol versions.
        let mut capabilities = advertisement.capabilities.clone();
        capabilities.extend(refs.derived_capabilities());
        Ok(DiscoveryResult {
            refs: refs.refs,
            capabilities,
            // A v2 advertisement carries no shallow boundary list; boundaries
            // arrive in each fetch response's `shallow-info` section instead.
            shallow_boundaries: Vec::new(),
            hash_kind: advertisement.hash_kind,
            version: ProtocolVersion::V2,
        })
    }

    /// POST a protocol v2 `ls-refs` request and return the raw response body.
    async fn post_ls_refs(&self, body: bytes::Bytes) -> Result<bytes::Bytes, GitError> {
        // INVARIANT: "git-upload-pack" is a valid relative URL onto self.url.
        let url = self
            .url
            .join("git-upload-pack")
            .expect("'git-upload-pack' is a valid relative URL");
        let res = BasicAuth::send(|| {
            let url = url.clone();
            let body = body.clone();
            async move {
                self.client
                    .post(url)
                    .header(GIT_PROTOCOL_HEADER, GIT_PROTOCOL_VERSION_2)
                    .header(CONTENT_TYPE, "application/x-git-upload-pack-request")
                    .header(
                        reqwest::header::ACCEPT,
                        "application/x-git-upload-pack-result",
                    )
                    .body(body)
            }
        })
        .await
        .map_err(|error| {
            GitError::NetworkError(format!(
                "Failed to send ls-refs request: {}",
                redact_url_credentials(&error.to_string())
            ))
        })?;
        if res.status() != 200 && res.status() != 304 {
            return Err(GitError::NetworkError(format!(
                "ls-refs request failed with status code: {}",
                res.status()
            )));
        }
        let body = res.bytes().await.map_err(|error| {
            GitError::NetworkError(format!(
                "Failed to read ls-refs response body: {}",
                redact_url_credentials(&error.to_string())
            ))
        })?;
        tracing::debug!("ls-refs response body: {} bytes", body.len());
        Ok(body)
    }

    /// POST $GIT_URL/git-upload-pack HTTP/1.0<br>
    /// Fetch the objects from the remote repository, which is specified by `have` and `want`.<br>
    /// `have` is the list of objects' hashes that the client already has, and `want` is the list of objects that the client wants.
    /// Obtain the `want` references from the `discovery_reference` method.<br>
    /// If the returned stream is empty, it may be due to incorrect refs or an incorrect format.
    /// `depth` is optional, if `Some(n)`, create a shallow clone with history truncated to n commits.
    pub async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        let discovery = self
            .discovery_reference(ServiceType::UploadPack)
            .await
            .map_err(|error| {
                IoError::other(format!(
                    "failed to discover HTTPS upload-pack capabilities: {error}"
                ))
            })?;
        self.fetch_objects_with_capabilities(
            have,
            want,
            shallow,
            depth,
            &discovery.capabilities,
            discovery.version,
            discovery.hash_kind,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_objects_with_capabilities(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
        advertised_capabilities: &[String],
        version: ProtocolVersion,
        hash_kind: HashKind,
    ) -> Result<FetchStream, IoError> {
        if version.is_v2() {
            // v2 replaced the want/have request line grammar with a `fetch`
            // command whose sections are delimited explicitly; the advertised
            // capability list is the same input the v0 builder consumes.
            let advertisement = v2::Advertisement {
                capabilities: advertised_capabilities.to_vec(),
                hash_kind,
            };
            let request = v2::fetch::FetchRequest {
                wants: want.to_vec(),
                haves: have.to_vec(),
                shallow: shallow.to_vec(),
                depth,
            };
            let body = v2::fetch::build_request(&advertisement, &request)?;
            return self.post_upload_pack_v2(body).await;
        }
        // POST $GIT_URL/git-upload-pack HTTP/1.0
        let body = generate_upload_pack_content_with_capabilities(
            have,
            want,
            shallow,
            depth,
            advertised_capabilities,
        )?;
        self.post_upload_pack(body).await
    }

    /// POST a protocol v2 `fetch` request and adapt the sectioned response into
    /// the v0-shaped frame sequence the fetch pipeline consumes.
    pub(crate) async fn post_upload_pack_v2(
        &self,
        body: bytes::Bytes,
    ) -> Result<FetchStream, IoError> {
        let stream = self
            .post_upload_pack_inner(body, ProtocolVersion::V2)
            .await?;
        Ok(v2::fetch::adapt_response_stream(stream))
    }

    /// POST a caller-built upload-pack request and stream the raw response.
    pub(crate) async fn post_upload_pack(
        &self,
        body: bytes::Bytes,
    ) -> Result<FetchStream, IoError> {
        self.post_upload_pack_inner(body, ProtocolVersion::V0).await
    }

    async fn post_upload_pack_inner(
        &self,
        body: bytes::Bytes,
        version: ProtocolVersion,
    ) -> Result<FetchStream, IoError> {
        // INVARIANT: "git-upload-pack" is a valid relative URL onto self.url.
        let url = self
            .url
            .join("git-upload-pack")
            .expect("'git-upload-pack' is a valid relative URL");
        tracing::debug!("fetch_objects with body: {:?}", body);

        let res = BasicAuth::send(|| {
            let url = url.clone();
            let body = body.clone();
            async move {
                let mut request = self
                    .client
                    .post(url)
                    .header("Content-Type", "application/x-git-upload-pack-request");
                if version.is_v2() {
                    // v2 POST bodies are only understood when the request also
                    // announces the version; the header is not implied by the body.
                    request = request.header(GIT_PROTOCOL_HEADER, GIT_PROTOCOL_VERSION_2);
                }
                request.body(body)
            }
        })
        .await
        .map_err(|e| {
            IoError::other(format!(
                "Failed to send request: {}",
                redact_url_credentials(&e.to_string())
            ))
        })?;
        // Never log the `Response` via `Debug` (embeds a possibly credentialed
        // URL); log status + a redacted URL instead.
        tracing::debug!(
            "upload-pack response: status={}, url={}",
            res.status(),
            redact_url_credentials(res.url().as_str())
        );

        if res.status() != 200 && res.status() != 304 {
            tracing::error!(
                "upload-pack request failed: status={}, url={}",
                res.status(),
                redact_url_credentials(res.url().as_str())
            );
            return Err(IoError::other(format!(
                "Error Response format, status code: {}",
                res.status()
            )));
        }
        let result = res.bytes_stream().map_err(std::io::Error::other).boxed();

        Ok(result)
    }

    pub async fn send_pack(&self, data: bytes::Bytes) -> Result<Response, reqwest::Error> {
        // INVARIANT: "git-receive-pack" is a valid relative URL onto self.url.
        let receive_pack_url = self
            .url
            .join("git-receive-pack")
            .expect("'git-receive-pack' is a valid relative URL");
        BasicAuth::send(|| async {
            self.client
                .post(receive_pack_url.clone())
                .header(CONTENT_TYPE, "application/x-git-receive-pack-request")
                .header(
                    reqwest::header::ACCEPT,
                    "application/x-git-receive-pack-result",
                )
                .header(reqwest::header::USER_AGENT, "git/2.43.0 libra")
                .header(reqwest::header::CONTENT_LENGTH, data.len())
                .body(data.clone())
        })
        .await
    }
}

/// Resolve the wire protocol version to negotiate for `service`.
///
/// receive-pack keeps using v0/v1 unconditionally: protocol v2 defines no push
/// commands, so announcing it there would change nothing a server can honour.
/// upload-pack consults the [`PROTOCOL_VERSION_CONFIG_KEY`] switch, defaulting to
/// v2 with automatic fallback when the server answers with a legacy
/// advertisement.
async fn preferred_protocol_version(service: ServiceType) -> ProtocolVersion {
    if service != ServiceType::UploadPack {
        return ProtocolVersion::V0;
    }
    let configured = match crate::internal::config::ConfigKv::get(PROTOCOL_VERSION_CONFIG_KEY).await
    {
        Ok(entry) => entry.map(|entry| entry.value.trim().to_string()),
        Err(error) => {
            // A config read failure must not silently disable v2: log and keep
            // the documented default so the fetch still works.
            tracing::debug!("could not read {PROTOCOL_VERSION_CONFIG_KEY}: {error}");
            None
        }
    };
    match configured.as_deref() {
        Some(other) if protocol_version_for_value(other).is_none() => {
            emit_warning(format!(
                "unrecognized {PROTOCOL_VERSION_CONFIG_KEY} value '{other}' (expected 0, 1 or 2); using protocol v2"
            ));
            ProtocolVersion::V2
        }
        value => protocol_version_for_value(value.unwrap_or("2")).unwrap_or(ProtocolVersion::V2),
    }
}

/// Map a `protocol.version` value to the wire version it selects.
///
/// `None` means the value is not a recognized version selector.
fn protocol_version_for_value(value: &str) -> Option<ProtocolVersion> {
    match value.trim() {
        "0" | "1" => Some(ProtocolVersion::V0),
        "2" => Some(ProtocolVersion::V2),
        _ => None,
    }
}

fn normalize_url(url: &Url) -> Url {
    if url.path().ends_with('/') {
        url.clone()
    } else {
        let mut url = url.clone();
        url.set_path(&format!("{}/", url.path()));
        url
    }
}

/// Redirect policy that refuses an https→http downgrade: reqwest strips
/// Authorization only when the HOST or PORT changes — a same-host cleartext
/// downgrade would re-send credentials over http (lore.md 1.6 review
/// finding). Same-scheme redirects keep reqwest's default 10-hop limit.
pub(crate) fn no_downgrade_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let downgraded = attempt.url().scheme() == "http"
            && attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.scheme() == "https");
        if downgraded {
            attempt.error("refusing an https->http downgrade redirect")
        } else if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// Loopback must bypass the OS/system HTTP proxy.
///
/// `Cargo.toml` sets `reqwest` `default-features = false`, but feature
/// unification with other crates re-enables `system-proxy`. Without this
/// guard, macOS system proxies (e.g. Clash/mihomo on `127.0.0.1:7897`)
/// intercept mock-server tests and return `502 Bad Gateway`.
pub(crate) fn url_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn build_client(
    connect_timeout: Duration,
    read_timeout: Duration,
    bypass_proxy: bool,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .http1_only()
        .redirect(no_downgrade_redirect_policy())
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout);
    if bypass_proxy {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| format!("failed to build HTTPS client: {e}"))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::{
        Router,
        body::Bytes as BodyBytes,
        extract::State,
        http::{HeaderMap, HeaderValue, header},
        routing::{get, post},
    };
    use bytes::BytesMut;
    use git_internal::hash::HashKind;
    use url::Url;

    use super::*;
    use crate::git_protocol::{add_delim_pkt, add_flush_pkt, add_pkt_line_string};

    /// Records the `Git-Protocol` request header of every inbound request.
    #[derive(Clone, Default)]
    struct Recorded {
        headers: Arc<std::sync::Mutex<Vec<Option<String>>>>,
        bodies: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl Recorded {
        fn record(&self, headers: &HeaderMap, body: &[u8]) {
            let value = headers
                .get(GIT_PROTOCOL_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            self.headers
                .lock()
                .expect("header recorder lock")
                .push(value);
            self.bodies
                .lock()
                .expect("body recorder lock")
                .push(body.to_vec());
        }

        fn headers(&self) -> Vec<Option<String>> {
            self.headers.lock().expect("header recorder lock").clone()
        }

        fn bodies(&self) -> Vec<Vec<u8>> {
            self.bodies.lock().expect("body recorder lock").clone()
        }
    }

    #[derive(Clone)]
    struct MockState {
        recorded: Recorded,
        advertisement: Vec<u8>,
        ls_refs: Vec<u8>,
        fetch: Vec<u8>,
    }

    fn v2_advertisement() -> Vec<u8> {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "# service=git-upload-pack\n".to_string());
        add_flush_pkt(&mut body);
        add_pkt_line_string(&mut body, "version 2\n".to_string());
        add_pkt_line_string(&mut body, "agent=git/2.43.0\n".to_string());
        add_pkt_line_string(&mut body, "ls-refs=unborn\n".to_string());
        add_pkt_line_string(&mut body, "fetch=shallow wait-for-done\n".to_string());
        add_pkt_line_string(&mut body, "object-format=sha1\n".to_string());
        add_flush_pkt(&mut body);
        body.to_vec()
    }

    fn v0_advertisement() -> Vec<u8> {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "# service=git-upload-pack\n".to_string());
        add_flush_pkt(&mut body);
        add_pkt_line_string(
            &mut body,
            format!(
                "{} HEAD\0side-band-64k symref=HEAD:refs/heads/main agent=git/2.43.0\n",
                "a".repeat(40)
            ),
        );
        add_pkt_line_string(&mut body, format!("{} refs/heads/main\n", "a".repeat(40)));
        add_flush_pkt(&mut body);
        body.to_vec()
    }

    fn ls_refs_response() -> Vec<u8> {
        let mut body = BytesMut::new();
        add_pkt_line_string(
            &mut body,
            format!("{} HEAD symref-target:refs/heads/main\n", "a".repeat(40)),
        );
        add_pkt_line_string(&mut body, format!("{} refs/heads/main\n", "a".repeat(40)));
        add_pkt_line_string(
            &mut body,
            format!(
                "{} refs/tags/v1 peeled:{}\n",
                "b".repeat(40),
                "c".repeat(40)
            ),
        );
        add_flush_pkt(&mut body);
        body.to_vec()
    }

    fn v2_fetch_response() -> Vec<u8> {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "acknowledgments\n".to_string());
        add_pkt_line_string(&mut body, "NAK\n".to_string());
        add_delim_pkt(&mut body);
        add_pkt_line_string(&mut body, "packfile\n".to_string());
        let pack = b"\x01PACKpayload";
        body.extend_from_slice(format!("{:04x}", pack.len() + 4).as_bytes());
        body.extend_from_slice(pack);
        add_flush_pkt(&mut body);
        body.to_vec()
    }

    async fn serve(state: MockState) -> (Url, Recorded) {
        let recorded = state.recorded.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock smart HTTP server");
        let addr = listener.local_addr().expect("mock server address");
        let app = Router::new()
            .route(
                "/repo/info/refs",
                get(
                    |State(state): State<MockState>, headers: HeaderMap| async move {
                        state.recorded.record(&headers, &[]);
                        (
                            [(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static(
                                    "application/x-git-upload-pack-advertisement",
                                ),
                            )],
                            state.advertisement,
                        )
                    },
                ),
            )
            .route(
                "/repo/git-upload-pack",
                post(
                    |State(state): State<MockState>,
                     headers: HeaderMap,
                     body: BodyBytes| async move {
                        state.recorded.record(&headers, &body);
                        let is_fetch = body
                            .windows(b"command=fetch".len())
                            .any(|window| window == b"command=fetch");
                        let response = if is_fetch {
                            state.fetch
                        } else {
                            state.ls_refs
                        };
                        (
                            [(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static("application/x-git-upload-pack-result"),
                            )],
                            response,
                        )
                    },
                ),
            )
            .with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock smart HTTP");
        });
        (
            Url::parse(&format!("http://{addr}/repo/")).expect("mock repository URL"),
            recorded,
        )
    }

    fn client(url: &Url) -> HttpsClient {
        HttpsClient::from_url_with_timeouts(url, Duration::from_secs(5), Duration::from_secs(5))
            .expect("https client")
    }

    fn mock_state(recorded: Recorded, advertisement: Vec<u8>) -> MockState {
        MockState {
            recorded,
            advertisement,
            ls_refs: ls_refs_response(),
            fetch: v2_fetch_response(),
        }
    }

    #[tokio::test]
    async fn v2_discovery_sends_git_protocol_header_and_runs_ls_refs() {
        let state = mock_state(Recorded::default(), v2_advertisement());
        let (url, recorded) = serve(state).await;
        let client = client(&url);

        let discovery = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .expect("v2 discovery");

        assert_eq!(discovery.version, ProtocolVersion::V2);
        assert_eq!(discovery.hash_kind, HashKind::Sha1);
        // HEAD, refs/heads/main, the tag and its synthesized peeled entry.
        assert_eq!(discovery.refs.len(), 4);
        assert_eq!(discovery.refs[0].name(), "HEAD");
        assert_eq!(discovery.refs[1].name(), "refs/heads/main");
        assert_eq!(discovery.refs[2].name(), "refs/tags/v1");
        assert_eq!(discovery.refs[3].name(), "refs/tags/v1^{}");
        assert!(
            discovery
                .capabilities
                .contains(&"symref=HEAD:refs/heads/main".to_string()),
            "derived symref capability: {:?}",
            discovery.capabilities
        );
        assert!(
            discovery
                .capabilities
                .contains(&"fetch=shallow wait-for-done".to_string()),
            "advertised capabilities are preserved"
        );
        assert!(
            discovery
                .capabilities
                .contains(&"ls-refs=unborn".to_string()),
            "advertised capabilities are preserved"
        );

        let headers = recorded.headers();
        assert_eq!(headers.len(), 2, "GET discovery plus POST ls-refs");
        assert_eq!(headers[0].as_deref(), Some(GIT_PROTOCOL_VERSION_2));
        assert_eq!(headers[1].as_deref(), Some(GIT_PROTOCOL_VERSION_2));
        let bodies = recorded.bodies();
        assert!(
            String::from_utf8_lossy(&bodies[1]).contains("command=ls-refs"),
            "POST body must be a ls-refs request"
        );
    }

    #[tokio::test]
    async fn v0_advertisement_falls_back_to_the_legacy_parser() {
        let state = mock_state(Recorded::default(), v0_advertisement());
        let (url, recorded) = serve(state).await;
        let client = client(&url);

        let discovery = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .expect("legacy advertisement");

        assert_eq!(discovery.version, ProtocolVersion::V0);
        assert!(discovery.refs.iter().any(|r| r.name() == "HEAD"));
        assert!(
            discovery
                .capabilities
                .contains(&"symref=HEAD:refs/heads/main".to_string()),
            "v0 capabilities stay untouched: {:?}",
            discovery.capabilities
        );
        // The client still announces v2 on the GET: fallback is the server's choice.
        assert_eq!(recorded.headers().len(), 1);
        assert_eq!(
            recorded.headers()[0].as_deref(),
            Some(GIT_PROTOCOL_VERSION_2)
        );
    }

    #[tokio::test]
    async fn v2_fetch_posts_command_and_adapts_the_response_stream() {
        let state = mock_state(Recorded::default(), v2_advertisement());
        let (url, recorded) = serve(state).await;
        let client = client(&url);
        let discovery = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .expect("v2 discovery");

        let capabilities = discovery.capabilities.clone();
        let mut stream = client
            .fetch_objects_with_capabilities(
                &["b".repeat(40)],
                &["a".repeat(40)],
                &[],
                None,
                &capabilities,
                ProtocolVersion::V2,
                discovery.hash_kind,
            )
            .await
            .expect("v2 fetch");
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.expect("streamed fetch frame"));
        }

        let headers = recorded.headers();
        assert_eq!(headers.len(), 3, "GET discovery, POST ls-refs, POST fetch");
        assert_eq!(
            headers[2].as_deref(),
            Some(GIT_PROTOCOL_VERSION_2),
            "the fetch POST must announce protocol v2"
        );
        let fetch_body = String::from_utf8_lossy(&recorded.bodies()[2]).to_string();
        assert!(fetch_body.contains("command=fetch"), "{fetch_body}");
        assert!(fetch_body.contains("done\n"), "{fetch_body}");
        assert!(!fetch_body.contains("thin-pack"), "{fetch_body}");

        // The adapted stream carries the packfile sideband frame and drops the
        // section headers the v0 consumer cannot parse.
        assert!(body.windows(5).any(|window| window == b"PACKp"), "{body:?}");
        assert!(!body.windows(8).any(|window| window == b"packfile"));
    }

    #[test]
    fn protocol_version_config_mapping() {
        assert_eq!(protocol_version_for_value("0"), Some(ProtocolVersion::V0));
        assert_eq!(protocol_version_for_value("1"), Some(ProtocolVersion::V0));
        assert_eq!(protocol_version_for_value("2"), Some(ProtocolVersion::V2));
        assert_eq!(protocol_version_for_value(" 2 "), Some(ProtocolVersion::V2));
        assert_eq!(protocol_version_for_value("v2"), None);
        assert_eq!(protocol_version_for_value(""), None);
    }

    #[tokio::test]
    async fn receive_pack_never_negotiates_v2() {
        // The version resolver is the only place that decides; receive-pack is
        // pinned to v0 because protocol v2 defines no receive-pack commands.
        assert_eq!(
            preferred_protocol_version(ServiceType::ReceivePack).await,
            ProtocolVersion::V0
        );
    }
}
