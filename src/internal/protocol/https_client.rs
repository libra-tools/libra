//! HTTPS smart protocol client that discovers refs, negotiates upload-pack/receive-pack, streams pack data, and supports basic authentication.

use std::{io::Error as IoError, ops::Deref, sync::Mutex, time::Duration};

use futures_util::{StreamExt, TryStreamExt};
use git_internal::errors::GitError;
use reqwest::{RequestBuilder, Response, StatusCode, header::CONTENT_TYPE};
use url::Url;

use super::{
    DiscoveryResult, FetchStream, ProtocolClient, generate_upload_pack_content,
    parse_discovered_references,
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

/// A Git protocol client that communicates with a Git server over HTTPS.
/// Only support `SmartProtocol` now, see [http-protocol](https://www.git-scm.com/docs/http-protocol) for protocol details.
pub struct HttpsClient {
    pub(crate) url: Url,
    pub(crate) client: reqwest::Client,
}

/// Default connection timeout for initial TCP+TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

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
        let client = build_client(connect_timeout, read_timeout)?;
        Ok(Self { url, client })
    }

    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, String> {
        self.client = build_client(connect_timeout, read_timeout)?;
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
            let send_result = BasicAuth::send(|| async { client.get(url_ref.clone()) }).await;
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

        parse_discovered_references(response_content, service)
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
        // POST $GIT_URL/git-upload-pack HTTP/1.0
        // INVARIANT: "git-upload-pack" is a valid relative URL onto self.url.
        let url = self
            .url
            .join("git-upload-pack")
            .expect("'git-upload-pack' is a valid relative URL");
        let body = generate_upload_pack_content(have, want, shallow, depth);
        tracing::debug!("fetch_objects with body: {:?}", body);

        let res = BasicAuth::send(|| async {
            self.client
                .post(url.clone())
                .header("Content-Type", "application/x-git-upload-pack-request")
                .body(body.clone())
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

fn build_client(
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .http1_only()
        .redirect(no_downgrade_redirect_policy())
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .build()
        .map_err(|e| format!("failed to build HTTPS client: {e}"))
}
