//! Remote media-capability probe (lore.md §6.4).
//!
//! GETs `<base>/libra/media/v1/capabilities` with the host-scoped bearer token,
//! wrapped in the §0.2 bounded backoff for 429/5xx. Classifies the result into a
//! [`ProbeOutcome`]; every ambiguity resolves to a SAFE outcome (a plain remote
//! reads as `NoEndpoint`, so negotiation falls back to standard LFS). Discovery
//! preserves the repository's standard LFS URL and uses a host-scoped token.

use futures_util::StreamExt;
use serde::Deserialize;

use super::negotiate::ProbeOutcome;
use crate::utils::backoff::{RetryOutcome, RetryPolicy, retry_idempotent};

/// The remote media capability document (§6.4). All fields are read defensively.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub chunked_lfs: bool,
    #[serde(default)]
    pub chunk_algorithms: Vec<String>,
    #[serde(default)]
    pub hash_algorithms: Vec<String>,
    #[serde(default)]
    pub max_chunk_size: u64,
    #[serde(default)]
    pub max_manifest_size: u64,
    #[serde(default)]
    pub supports_batch_exists: bool,
    #[serde(default)]
    pub supports_range_read: bool,
    #[serde(default)]
    pub supports_standard_lfs_fallback: bool,
}

/// How a single HTTP status maps to a probe step. Pure + unit-testable; the
/// async loop consults it before deciding to retry or terminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// 2xx — decode the body.
    Success,
    /// 429/5xx — transient; retry under §0.2 backoff.
    Retryable,
    /// Anything else (404/401/403/other 4xx) — no usable capability endpoint.
    NoEndpoint,
}

/// Classify an HTTP status code for the capability probe.
pub fn classify_status(status: u16) -> StatusClass {
    if (200..300).contains(&status) {
        StatusClass::Success
    } else if status == 429 || (500..600).contains(&status) {
        StatusClass::Retryable
    } else {
        StatusClass::NoEndpoint
    }
}

/// The capability endpoint path appended to a remote's media base URL.
pub const CAPABILITIES_PATH: &str = "libra/media/v1/capabilities";

/// Probe `base_url`'s media capability endpoint. Never errors — every failure
/// mode maps to a safe [`ProbeOutcome`]. A malformed/unreachable endpoint or a
/// non-2xx-non-5xx status yields `NoEndpoint`; a 429/5xx that survives §0.2
/// retries yields `ServerErrorAfterBackoff`.
pub async fn probe(base_url: &str) -> ProbeOutcome {
    let Ok(client) = reqwest::Client::builder()
        .redirect(crate::internal::protocol::https_client::no_downgrade_redirect_policy())
        .build()
    else {
        return ProbeOutcome::NoEndpoint;
    };
    probe_with_client(base_url, client).await
}

/// The supplied client must refuse HTTPS-to-HTTP redirects before forwarding
/// stored credentials. LFSClient and `probe` use the shared redirect policy.
pub async fn probe_with_client(base_url: &str, client: reqwest::Client) -> ProbeOutcome {
    let url = match join_capabilities_url(base_url) {
        Some(u) => u,
        None => return ProbeOutcome::NoEndpoint,
    };
    // Resolve an optional host-scoped bearer token (auth is best-effort here).
    let token = match crate::internal::auth::HostScope::from_request_url(&url) {
        Some(scope) => match crate::internal::auth::lookup(&scope).await {
            crate::internal::auth::Lookup::Valid { token, .. } => Some(token),
            _ => None,
        },
        None => None,
    };

    let result: Result<ProbeOutcome, ()> = retry_idempotent(&RetryPolicy::default(), |_attempt| {
        let url = url.clone();
        let token = token.clone();
        let client = client.clone();
        async move {
            let mut req = client
                .get(url)
                .header("Accept", "application/json")
                .timeout(std::time::Duration::from_secs(10));
            if let Some(t) = &token {
                req = req.bearer_auth(t);
            }
            match req.send().await {
                Ok(resp) => match classify_status(resp.status().as_u16()) {
                    StatusClass::Success => match decode_capabilities(resp).await {
                        Some(caps) => RetryOutcome::Done(Ok(ProbeOutcome::Ok(caps))),
                        // A 2xx with an undecodable body is not a usable
                        // capability endpoint — fall back safely.
                        None => RetryOutcome::Done(Ok(ProbeOutcome::NoEndpoint)),
                    },
                    StatusClass::NoEndpoint => RetryOutcome::Done(Ok(ProbeOutcome::NoEndpoint)),
                    StatusClass::Retryable => RetryOutcome::Retry {
                        retry_after: None,
                        last_err: (),
                    },
                },
                // Connection refused / DNS / TLS: no reachable endpoint.
                Err(e) if e.is_connect() || e.is_request() => {
                    RetryOutcome::Done(Ok(ProbeOutcome::NoEndpoint))
                }
                // Timeouts and other transient transport errors: retry.
                Err(_) => RetryOutcome::Retry {
                    retry_after: None,
                    last_err: (),
                },
            }
        }
    })
    .await;

    // Exhausted retries on a 429/5xx (or persistent transient error) → distinct
    // reason so negotiation records ServerErrorAfterBackoff, not NoEndpoint.
    result.unwrap_or(ProbeOutcome::ServerErrorAfterBackoff)
}

async fn decode_capabilities(response: reqwest::Response) -> Option<Capabilities> {
    let mut data = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.ok()?;
        if bytes.len() > (16 * 1024usize).saturating_sub(data.len()) {
            return None;
        }
        data.extend_from_slice(&bytes);
    }
    serde_json::from_slice(&data).ok()
}

/// Join the media base URL with the capabilities path, tolerating a trailing
/// slash. Returns `None` for an unparseable base.
fn join_capabilities_url(base_url: &str) -> Option<url::Url> {
    let lfs_url = crate::utils::lfs::generate_lfs_server_url(base_url.to_owned());
    let mut url = url::Url::parse(&lfs_url).ok()?;
    url.set_path(&format!(
        "{}/{CAPABILITIES_PATH}",
        url.path().trim_end_matches('/')
    ));
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification() {
        assert_eq!(classify_status(200), StatusClass::Success);
        assert_eq!(classify_status(204), StatusClass::Success);
        assert_eq!(classify_status(404), StatusClass::NoEndpoint);
        assert_eq!(classify_status(401), StatusClass::NoEndpoint);
        assert_eq!(classify_status(403), StatusClass::NoEndpoint);
        assert_eq!(classify_status(429), StatusClass::Retryable);
        assert_eq!(classify_status(500), StatusClass::Retryable);
        assert_eq!(classify_status(503), StatusClass::Retryable);
    }

    #[test]
    fn joins_capability_url() {
        let u = join_capabilities_url("https://host.example/repo").unwrap();
        assert_eq!(
            u.as_str(),
            "https://host.example/repo.git/info/lfs/libra/media/v1/capabilities"
        );
        let u = join_capabilities_url("https://host.example/repo/").unwrap();
        assert_eq!(
            u.as_str(),
            "https://host.example/repo.git/info/lfs/libra/media/v1/capabilities"
        );
        let u =
            join_capabilities_url("ssh://git@host.example:8443/repo.git/info/lfs/?tenant=one#ref")
                .unwrap();
        assert_eq!(
            u.as_str(),
            "https://host.example:8443/repo.git/info/lfs/libra/media/v1/capabilities?tenant=one"
        );
        assert!(join_capabilities_url("not a url").is_none());
    }
}
