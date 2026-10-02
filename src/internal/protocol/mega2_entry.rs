//! Bounded `POST /api/v1/create-entry` directory client (plan-20260912 MB-04).
//!
//! The only write route Libra consumes here is `create-entry`, and this module
//! only ever asks it to create a **directory**: `is_directory=true`, a locally
//! validated rooted parent `path`, a validated single-segment `name`, no
//! `content`, `skip_build=true` and no `mode`/`author_email`. Everything is
//! validated before a request is built, responses are bounded, and a failure
//! never triggers a retry or a fallback route.
//!
//! A caller-supplied token becomes exactly one `Authorization: Bearer …`
//! header; without a token the request carries no `Authorization` header at
//! all (anonymous `push_auth=none` deployments).

use std::time::Duration;

use reqwest::{StatusCode, header::AUTHORIZATION};
use serde::{Deserialize, Serialize};
use url::Url;

use super::mega2_diag;
use crate::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_tree::{
            MAX_NAME_BYTES, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, normalize_path,
            validate_server_url,
        },
    },
    utils::error::{CliError, CliResult, StableErrorCode},
};

/// The single write route this client talks to.
pub const CREATE_ENTRY_ROUTE: &str = "/api/v1/create-entry";

#[derive(Debug, Serialize)]
struct CreateEntryBody<'a> {
    is_directory: bool,
    name: &'a str,
    path: &'a str,
    content: Option<&'a str>,
    skip_build: bool,
}

#[derive(Debug, Deserialize)]
struct WireCommonResult<T> {
    req_result: bool,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct WireCreateEntryResult {
    commit_id: String,
    new_oid: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    cl_link: Option<String>,
}

/// What a successful remote directory creation proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCreateReceipt {
    /// Commit created by the server (never empty on success).
    pub commit_id: String,
    /// Oid of the created entry (never empty on success).
    pub new_oid: String,
    /// Server-reported path — informational only, never navigation authority.
    pub path: Option<String>,
    /// Server CL link, if the deployment produced one.
    pub cl_link: Option<String>,
}

/// Validates one directory entry name (a single path segment).
pub fn validate_entry_name(name: &str) -> CliResult<()> {
    if name.is_empty() {
        return Err(CliError::fatal("mega2 entry name must not be empty")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(CliError::fatal(format!(
            "mega2 entry name exceeds the {MAX_NAME_BYTES}-byte limit"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name == "." || name == ".." {
        return Err(CliError::fatal(format!(
            "mega2 entry name '{name}' is not a valid directory name"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(
            CliError::fatal("mega2 entry name must not contain path separators")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    if name.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(
            CliError::fatal("mega2 entry name contains NUL or control characters")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    Ok(())
}

/// One-shot directory creation client.
#[derive(Debug)]
pub struct Mega2EntryClient {
    base: Url,
    http: reqwest::Client,
    token: Option<Mega2Token>,
}

impl Mega2EntryClient {
    /// Builds the client with the default MB-01 timeout.
    pub fn new(server_url: &str, token: Option<Mega2Token>) -> CliResult<Self> {
        Self::with_timeouts(server_url, token, REQUEST_TIMEOUT)
    }

    /// Test seam: same builder with a caller-provided deadline.
    pub fn with_timeouts(
        server_url: &str,
        token: Option<Mega2Token>,
        timeout: Duration,
    ) -> CliResult<Self> {
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
        Ok(Self { base, http, token })
    }

    /// Whether this client will attach an `Authorization` header.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Creates one directory under `parent_path`; exactly one request.
    ///
    /// `parent_path` is validated with the shared MB-01 path rules (rooted,
    /// no traversal) and sent verbatim, so `/` creates at the root.
    pub async fn create_directory(
        &self,
        parent_path: &str,
        name: &str,
    ) -> CliResult<RemoteCreateReceipt> {
        mega2_diag::run(
            mega2_diag::CREATE_ENTRY,
            self.create_directory_scoped(parent_path, name),
        )
        .await
    }

    /// Body of [`Self::create_directory`]; runs inside its diagnostics scope.
    async fn create_directory_scoped(
        &self,
        parent_path: &str,
        name: &str,
    ) -> CliResult<RemoteCreateReceipt> {
        let parent = normalize_path(parent_path)?;
        validate_entry_name(name)?;

        let url = self.base.join(CREATE_ENTRY_ROUTE).map_err(|_| {
            CliError::fatal("cannot build the mega2 create-entry URL")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        let body = CreateEntryBody {
            is_directory: true,
            name,
            path: &parent,
            content: None,
            skip_build: true,
        };

        let mut request = self.http.post(url).json(&body);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token.expose()));
        }

        let response = mega2_diag::send(request, transport_error).await?;
        let status = response.status();
        match status {
            StatusCode::UNAUTHORIZED => {
                return Err(CliError::fatal(
                    "mega2 server requires a write token for create-entry (HTTP 401)",
                )
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint(
                    "provide a token with --token-file <path> (or LIBRA_MEGA2_TOKEN) allowed to write this path",
                ));
            }
            StatusCode::FORBIDDEN => {
                return Err(CliError::fatal(
                    "mega2 write token is not authorized for this path (HTTP 403)",
                )
                .with_stable_code(StableErrorCode::AuthPermissionDenied));
            }
            StatusCode::CONFLICT => {
                return Err(
                    CliError::fatal("mega2 server refused the create request (HTTP 409)")
                        .with_stable_code(StableErrorCode::ConflictOperationBlocked),
                );
            }
            StatusCode::BAD_REQUEST => {
                return Err(CliError::fatal(
                    "mega2 server rejected the create request (HTTP 400) — the name may already exist",
                )
                .with_stable_code(StableErrorCode::CliInvalidTarget));
            }
            status if status.is_redirection() => {
                return Err(CliError::fatal(format!(
                    "mega2 server redirected the create request (HTTP {status}) — redirects are refused"
                ))
                .with_stable_code(StableErrorCode::NetworkProtocol));
            }
            status if !status.is_success() => {
                return Err(CliError::fatal(format!(
                    "mega2 server returned HTTP {status} for create-entry"
                ))
                .with_stable_code(StableErrorCode::NetworkProtocol));
            }
            _ => {}
        }

        let raw = read_bounded(response).await?;
        let envelope: WireCommonResult<WireCreateEntryResult> = serde_json::from_slice(&raw)
            .map_err(|_| {
                CliError::fatal("mega2 server returned an invalid create-entry response")
                    .with_stable_code(StableErrorCode::NetworkProtocol)
            })?;
        if !envelope.req_result {
            return Err(
                CliError::fatal("mega2 create-entry failed (req_result=false)")
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            );
        }
        let data = envelope.data.ok_or_else(|| {
            CliError::fatal("mega2 create-entry response carried no data")
                .with_stable_code(StableErrorCode::NetworkProtocol)
        })?;
        if data.commit_id.is_empty() || data.new_oid.is_empty() {
            return Err(CliError::fatal(
                "mega2 create-entry response is missing commit_id/new_oid",
            )
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        Ok(RemoteCreateReceipt {
            commit_id: data.commit_id,
            new_oid: data.new_oid,
            path: data.path,
            cl_link: data.cl_link,
        })
    }
}

fn transport_error(error: reqwest::Error) -> CliError {
    if error.is_timeout() {
        CliError::fatal("mega2 create-entry request timed out")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else if error.is_connect() {
        CliError::fatal("cannot connect to the mega2 server")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else {
        CliError::fatal("mega2 create-entry request failed")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    }
}

/// Streams the body with an immediate abort past [`MAX_RESPONSE_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> CliResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(CliError::fatal(format!(
                "mega2 create-entry response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_reject_traversal_separators_and_control() {
        for bad in ["", ".", "..", "a/b", "a\\b", "nul\0name", "bell\u{7}"] {
            assert!(
                validate_entry_name(bad).is_err(),
                "expected refusal for {bad:?}"
            );
        }
        assert!(
            validate_entry_name(&"a".repeat(MAX_NAME_BYTES + 1)).is_err(),
            "over-long name refused"
        );
        for ok in ["src", "a b", "dots...", "unicode-目錄"] {
            assert!(
                validate_entry_name(ok).is_ok(),
                "expected accept for {ok:?}"
            );
        }
    }

    #[test]
    fn directory_body_shape_omits_file_only_fields() {
        let body = CreateEntryBody {
            is_directory: true,
            name: "pkg",
            path: "/src",
            content: None,
            skip_build: true,
        };
        let json = serde_json::to_value(&body).expect("serialize");
        assert_eq!(json["is_directory"], true);
        assert_eq!(json["name"], "pkg");
        assert_eq!(json["path"], "/src");
        assert_eq!(json["skip_build"], true);
        assert_eq!(json["content"], serde_json::Value::Null);
        assert!(json.get("mode").is_none(), "mode must be omitted");
        assert!(json.get("author_email").is_none(), "email omitted");
        assert!(json.get("author_username").is_none(), "username omitted");
    }
}
