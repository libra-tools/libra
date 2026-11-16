//! Typed Mega2 TraceRepo Git transport shared by `agent push --mega2` and
//! `mega2 traces fetch`.
//!
//! The stored code remote is only read. Each call derives a locator URL on
//! the same origin: HTTP(S) authenticates with the Mega2 token as Basic
//! password under login `traces`; SSH uses the reserved `traces` login and
//! the key resolved by the ordinary SSH configuration (`LIBRA_SSH_COMMAND`,
//! vault key, or the system agent).

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use git_internal::errors::GitError;

use crate::{
    command::fetch::RemoteClient,
    git_protocol::{ServiceType, add_pkt_line_string},
    internal::protocol::{
        DiscoveryResult, https_client::BasicAuth, mega2_auth::Mega2Token, mega2_traces,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

/// Discovery and connection timeout.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Idle timeout for pack transfer.
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Largest PACK the TraceRepo accepts or serves (`max_pack_bytes`).
pub(crate) const MAX_PACK_BYTES: usize = 536_870_912;

/// Build a client for `locator_path` on the origin of `remote_url`.
pub(crate) async fn open_client(
    remote_url: &str,
    remote: &str,
    locator_path: &str,
    token: Option<&Mega2Token>,
    output: &OutputConfig,
) -> CliResult<RemoteClient> {
    let ssh = remote_url.starts_with("ssh://");
    let transport_url = if ssh {
        mega2_traces::traces_ssh_url(remote_url, locator_path)
    } else {
        mega2_traces::traces_http_url(remote_url, locator_path)
    }
    .map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let client = RemoteClient::from_spec_with_remote_and_confirmation(
        &transport_url,
        Some(remote),
        crate::internal::protocol::ssh_client::host_key_confirmation_for_output(output),
    )
    .await
    .map_err(|message| {
        CliError::fatal(format!("cannot build the traces transport: {message}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let client = match client {
        RemoteClient::Ssh(client) => {
            RemoteClient::Ssh(client.with_literal_repo_path(locator_path.to_string()))
        }
        RemoteClient::Http(client) => RemoteClient::Http(client),
        _ => {
            return Err(
                CliError::fatal("Mega2 traces only use HTTP(S) or SSH".to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget),
            );
        }
    }
    .with_network_timeouts(CONNECT_TIMEOUT, IDLE_TIMEOUT)
    .map_err(|message| {
        CliError::network(format!("cannot configure the traces transport: {message}"))
    })?;
    if let Some(token) = token {
        BasicAuth::set_auth(BasicAuth {
            username: "traces".to_string(),
            password: token.expose().to_string(),
        })
        .await;
    }
    Ok(client)
}

/// Authenticated Git discovery on a traces locator.
pub(crate) async fn discover(
    client: &RemoteClient,
    service: ServiceType,
) -> CliResult<DiscoveryResult> {
    retry_busy(|| discover_once(client, service)).await
}

async fn discover_once(client: &RemoteClient, service: ServiceType) -> CliResult<DiscoveryResult> {
    match tokio::time::timeout(CONNECT_TIMEOUT, client.discovery_reference(service)).await {
        Err(_) => Err(CliError::network(format!(
            "traces discovery timed out after {}s",
            CONNECT_TIMEOUT.as_secs()
        ))
        .with_stable_code(StableErrorCode::NetworkUnavailable)),
        Ok(Ok(discovery)) => Ok(discovery),
        Ok(Err(GitError::UnAuthorized(_))) => Err(CliError::auth(
            "the traces remote rejected the credential (unauthenticated)",
        )
        .with_stable_code(StableErrorCode::AuthPermissionDenied)
        .with_hint("check LIBRA_MEGA2_TOKEN / --token-file, or the SSH key for this remote")),
        Ok(Err(error)) => Err(remote_error("traces discovery", &error.to_string())),
    }
}

/// Classify a transport failure text from the TraceRepo.
pub(crate) fn remote_error(phase: &str, detail: &str) -> CliError {
    let detail = crate::utils::redact::redact_url_credentials(detail);
    if detail.contains("status code: 404") || detail.contains("traces locator not found") {
        return CliError::auth(format!(
            "{phase} was refused with unknown_scope: the scope does not exist for this credential, or the credential may not use it"
        ))
        .with_stable_code(StableErrorCode::AuthPermissionDenied)
        .with_hint("only the owner can update a stream; check the project path and credential");
    }
    if detail.contains("status code: 401") {
        return CliError::auth(format!("{phase} was refused: unauthenticated"))
            .with_stable_code(StableErrorCode::AuthPermissionDenied);
    }
    if detail.contains("Permission denied") || detail.contains("publickey") {
        return CliError::auth(format!("{phase} failed: SSH authentication: {detail}"))
            .with_stable_code(StableErrorCode::AuthPermissionDenied);
    }
    CliError::network(format!("{phase} failed: {detail}"))
        .with_stable_code(StableErrorCode::NetworkUnavailable)
}

/// Attempts for a request the TraceRepo refused as busy.
const BUSY_ATTEMPTS: u32 = 8;

/// Whether the TraceRepo refused the request because its single receive or
/// fetch slot was taken (`query_timeout`, HTTP 503). Nothing was published.
pub(crate) fn is_busy(error: &CliError) -> bool {
    let message = error.message();
    message.contains("query_timeout") || message.contains("status code: 503")
}

/// Retry `attempt` with bounded exponential backoff while the TraceRepo is
/// busy. Publication is atomic and keyed by the old and new tip, so a
/// repeated receive cannot apply twice.
pub(crate) async fn retry_busy<T, F, Fut>(mut attempt: F) -> CliResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = CliResult<T>>,
{
    let mut delay = Duration::from_millis(200);
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(error) if is_busy(&error) && tries < BUSY_ATTEMPTS => {
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| u64::from(elapsed.subsec_nanos() % 200))
                    .unwrap_or(0);
                tracing::debug!(tries, "traces remote is busy; retrying");
                tokio::time::sleep(delay + Duration::from_millis(jitter)).await;
                delay *= 2;
                tries += 1;
            }
            other => return other,
        }
    }
}

/// One-want v0 upload request with only the capabilities the TraceRepo
/// accepts: an explicit SHA-256 format and the agent token.
pub(crate) fn upload_request(want: &str, kind: mega2_traces::HashKind) -> Bytes {
    let mut capabilities = String::new();
    if kind == mega2_traces::HashKind::Sha256 {
        capabilities.push_str(" object-format=sha256");
    }
    capabilities.push_str(&format!(" agent=libra/{}", env!("CARGO_PKG_VERSION")));
    let mut data = BytesMut::new();
    add_pkt_line_string(&mut data, format!("want {want}{capabilities}\n"));
    data.extend_from_slice(b"0000");
    add_pkt_line_string(&mut data, "done\n".to_string());
    data.freeze()
}

/// Read a raw upload-pack response: one `NAK` packet followed by PACK bytes.
pub(crate) async fn read_upload_response(
    client: &RemoteClient,
    request: Bytes,
) -> CliResult<Vec<u8>> {
    let mut stream = client
        .upload_pack_raw(request)
        .await
        .map_err(|error| remote_error("traces upload-pack", &error.to_string()))?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| remote_error("traces upload-pack", &error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > MAX_PACK_BYTES + 64 {
            return Err(CliError::fatal(format!(
                "traces archive exceeds the {MAX_PACK_BYTES} byte pack limit"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        body.extend_from_slice(&chunk);
    }
    split_upload_response(body)
}

pub(crate) fn split_upload_response(mut body: Vec<u8>) -> CliResult<Vec<u8>> {
    let invalid = |detail: String| {
        CliError::network(format!("traces upload-pack response is invalid: {detail}"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    };
    if body.len() < 4 {
        return Err(invalid("empty response".to_string()));
    }
    let header = std::str::from_utf8(&body[..4])
        .ok()
        .and_then(|text| usize::from_str_radix(text, 16).ok())
        .ok_or_else(|| invalid("missing pkt-line header".to_string()))?;
    if header < 4 || header > body.len() {
        return Err(invalid("truncated first packet".to_string()));
    }
    let line = String::from_utf8_lossy(&body[4..header])
        .trim_end()
        .to_string();
    if let Some(message) = line.strip_prefix("ERR ") {
        return Err(remote_error("traces upload-pack", message));
    }
    if line != "NAK" {
        return Err(invalid(format!("unexpected first packet {line:?}")));
    }
    let pack = body.split_off(header);
    if !pack.starts_with(b"PACK") {
        return Err(invalid("no PACK after NAK".to_string()));
    }
    Ok(pack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_upload_framing_matrix() {
        let request = upload_request(&"a".repeat(40), mega2_traces::HashKind::Sha1);
        let text = String::from_utf8_lossy(&request);
        assert!(text.contains(&format!("want {} agent=libra/", "a".repeat(40))));
        assert!(!text.contains("side-band"));
        assert!(text.ends_with("00000009done\n"));
        let sha256 = upload_request(&"b".repeat(64), mega2_traces::HashKind::Sha256);
        assert!(String::from_utf8_lossy(&sha256).contains("object-format=sha256"));

        let mut good = b"0008NAK\n".to_vec();
        good.extend_from_slice(b"PACK\0\0\0\x02");
        assert_eq!(split_upload_response(good).unwrap(), b"PACK\0\0\0\x02");
        assert!(split_upload_response(b"0008NAK\n".to_vec()).is_err());
        let denied =
            split_upload_response(b"0020ERR traces locator not found".to_vec()).expect_err("ERR");
        assert!(denied.message().contains("unknown_scope"));
        assert!(split_upload_response(b"0009ACK x".to_vec()).is_err());
        assert!(split_upload_response(Vec::new()).is_err());
    }

    #[tokio::test]
    async fn traces_busy_retry_matrix() {
        let busy =
            || CliError::conflict("traces remote rejected the push: query_timeout (HTTP 503)");
        assert!(is_busy(&busy()));
        assert!(is_busy(&remote_error(
            "traces upload-pack",
            "Error Response format, status code: 503"
        )));
        assert!(!is_busy(&CliError::conflict("object_integrity")));

        let calls = std::sync::atomic::AtomicU32::new(0);
        let value = retry_busy(|| async {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                Err(busy())
            } else {
                Ok(7)
            }
        })
        .await
        .expect("third attempt succeeds");
        assert_eq!(value, 7);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

        let other = std::sync::atomic::AtomicU32::new(0);
        let refused: CliResult<()> = retry_busy(|| async {
            other.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(CliError::conflict("object_integrity"))
        })
        .await;
        assert!(refused.is_err());
        assert_eq!(
            other.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a non-busy refusal is not retried"
        );
    }
}
