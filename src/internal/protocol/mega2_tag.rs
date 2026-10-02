//! Bounded `tag_router` client: list / create / get / delete
//! (plan-20260912 MB-10, DEP-MB-05, ADR-MB-06).
//!
//! The pinned contract is the storage-only `tag_router` mounted at
//! `api_router.rs:78` (`a1293686`):
//!
//! - `GET /api/v1/tags/list` — anonymous, with the **required** query keys
//!   `page`/`per_page`/`path`; the root MVP always sends `path="/"`;
//! - `POST /api/v1/tags` — trunk write, token via [`Mega2Token`], body
//!   `CreateTagRequest` (no `tagger` key; `message` present makes an annotated
//!   tag), any 2xx + `req_result=true` is success;
//! - `GET /api/v1/tags/{name}` — anonymous, optional `path` selector
//!   (root MVP sends `/`), used for conflict diagnosis;
//! - `DELETE /api/v1/tags/{name}` — trunk write, token via [`Mega2Token`],
//!   authorization path = the `path` selector (root MVP `/`).
//!
//! Everything is validated before a request is built, responses are bounded,
//! and 404 get/delete map to the stable not-found code
//! ([`StableErrorCode::CliInvalidTarget`]).

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use super::mega2_diag;
use crate::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_tree::{MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, normalize_path, validate_server_url},
    },
    utils::error::{CliError, CliResult, StableErrorCode},
};

/// Tag collection route (`POST` create).
pub const TAGS_ROUTE: &str = "/api/v1/tags";
/// Tag list route (anonymous `GET`, three required query keys).
pub const TAGS_LIST_ROUTE: &str = "/api/v1/tags/list";
/// The pinned DEP-MB-05 evidence this module was written against.
pub const DEP_MB_05_REFERENCE: &str =
    "mega2@a1293686 api_router.rs:78 (tag_router merged) + tag_router.rs:155-352";
/// Client-side page size cap (mirrors the documented list bound).
pub const MAX_PER_PAGE: u64 = 100;
/// Named maximum page number the client will request.
pub const MAX_PAGE: u64 = 1000;
/// Server tag-name length limit.
pub const MAX_TAG_NAME_BYTES: usize = 255;

/// Re-verified state of the mounted DEP-MB-05 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountedPin {
    /// Pin the verification was performed against.
    pub reference: &'static str,
    /// `tag_router::routers()` is merged into `storage_only_routers_with`.
    pub tag_router_merged_into_storage_only: bool,
    /// List is `GET /tags/list` with the three required query keys (not POST).
    pub list_is_get_with_required_query: bool,
    /// create/delete use the same `push_auth` kind as create-entry.
    pub push_auth_same_kind_as_create_entry: bool,
}

impl MountedPin {
    /// The verification performed for this card (ER-MB-02, 2026-09-21).
    pub fn verified_a1293686() -> Self {
        Self {
            reference: DEP_MB_05_REFERENCE,
            tag_router_merged_into_storage_only: true,
            list_is_get_with_required_query: true,
            push_auth_same_kind_as_create_entry: true,
        }
    }

    /// Whether this evidence satisfies the start gate.
    pub fn is_verified(&self) -> bool {
        self.reference == DEP_MB_05_REFERENCE
            && self.tag_router_merged_into_storage_only
            && self.list_is_get_with_required_query
            && self.push_auth_same_kind_as_create_entry
    }
}

/// Mirrors the server's `validate_tag_name`.
pub fn validate_tag_name(name: &str) -> CliResult<()> {
    if name.is_empty() {
        return Err(CliError::fatal("mega2 tag name must not be empty")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name.len() > MAX_TAG_NAME_BYTES {
        return Err(CliError::fatal(format!(
            "mega2 tag name exceeds the {MAX_TAG_NAME_BYTES}-byte limit"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name.contains("..") || name.contains("@{") || name.contains("//") {
        return Err(CliError::fatal(
            "mega2 tag name contains a reserved sequence ('..', '@{', '//')",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if name.ends_with(".lock") {
        return Err(CliError::fatal("mega2 tag name must not end with '.lock'")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    const FORBIDDEN: &[char] = &[' ', '~', '^', ':', '?', '*', '[', '\\'];
    for c in name.chars() {
        if FORBIDDEN.contains(&c) {
            return Err(CliError::fatal(format!(
                "mega2 tag name contains forbidden character '{c}'"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        if c == '\0' || c.is_control() {
            return Err(
                CliError::fatal("mega2 tag name contains NUL or control characters")
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
            );
        }
    }
    Ok(())
}

/// One tag as the server reports it (immutable data, receipt only).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TagInfo {
    pub name: String,
    pub tag_id: String,
    pub object_id: String,
    pub object_type: String,
    pub tagger: String,
    pub message: String,
    pub created_at: String,
}

/// One page of tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagPage {
    pub total: u64,
    pub items: Vec<TagInfo>,
}

/// Create request built only from validated caller inputs.
#[derive(Debug, Clone, Default)]
pub struct CreateTagOptions<'a> {
    pub name: &'a str,
    pub target: Option<&'a str>,
    pub path_context: Option<&'a str>,
    pub tagger_name: Option<&'a str>,
    pub tagger_email: Option<&'a str>,
    /// `Some` creates an annotated tag; `None` a lightweight one.
    pub message: Option<&'a str>,
}

/// What a successful delete proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTagReceipt {
    pub deleted_tag: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
struct CreateTagBody<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tagger_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tagger_email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct WireCommonResult<T> {
    req_result: bool,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct WirePage<T> {
    total: u64,
    items: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct WireDeleteTag {
    deleted_tag: String,
    #[serde(default)]
    message: String,
}

/// Validates list pagination locally (the server also rejects `per_page=0`).
fn validate_pagination(page: u64, per_page: u64) -> CliResult<()> {
    if page == 0 || page > MAX_PAGE {
        return Err(CliError::fatal(format!(
            "mega2 tag list page must be between 1 and {MAX_PAGE}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if per_page == 0 || per_page > MAX_PER_PAGE {
        return Err(CliError::fatal(format!(
            "mega2 tag list per_page must be between 1 and {MAX_PER_PAGE}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    Ok(())
}

fn create_body<'a>(options: &CreateTagOptions<'a>) -> CreateTagBody<'a> {
    CreateTagBody {
        name: options.name,
        target: options.target,
        // Root MVP: an omitted selector is sent explicitly as `/`.
        path_context: Some(options.path_context.unwrap_or("/")),
        tagger_name: options.tagger_name,
        tagger_email: options.tagger_email,
        message: options.message,
    }
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
                "mega2 server requires a write token for tag {operation} (HTTP 401)"
            ))
            .with_stable_code(StableErrorCode::AuthMissingCredentials)
            .with_hint(
                "provide a token with --token-file <path> (or LIBRA_MEGA2_TOKEN) allowed to write tags",
            ));
        }
        403 => {
            return Err(CliError::fatal(format!(
                "mega2 write token is not authorized for the tag {operation} path (HTTP 403)"
            ))
            .with_stable_code(StableErrorCode::AuthPermissionDenied));
        }
        404 => {
            return Err(CliError::fatal(format!(
                "mega2 tag {operation} target was not found (HTTP 404)"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
        409 => {
            return Err(CliError::fatal(format!(
                "mega2 server refused the tag {operation} request (HTTP 409)"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked));
        }
        400 | 405 | 422 => {
            return Err(CliError::fatal(format!(
                "mega2 server rejected the tag {operation} request (HTTP {status})"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        status if (300..400).contains(&status) => {
            return Err(CliError::fatal(format!(
                "mega2 server redirected the tag {operation} request (HTTP {status}) — redirects are refused"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        status if !(200..300).contains(&status) => {
            return Err(CliError::fatal(format!(
                "mega2 server returned HTTP {status} for tag {operation}"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol));
        }
        _ => {}
    }

    let envelope: WireCommonResult<T> = serde_json::from_slice(raw).map_err(|_| {
        CliError::fatal(format!(
            "mega2 server returned an invalid tag {operation} response"
        ))
        .with_stable_code(StableErrorCode::NetworkProtocol)
    })?;
    if !envelope.req_result {
        return Err(
            CliError::fatal(format!("mega2 tag {operation} failed (req_result=false)"))
                .with_stable_code(StableErrorCode::NetworkProtocol),
        );
    }
    envelope.data.ok_or_else(|| {
        CliError::fatal(format!("mega2 tag {operation} response carried no data"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    })
}

/// Tag client: anonymous list/get, token-bearing create/delete.
#[derive(Debug)]
pub struct Mega2TagClient {
    base: Url,
    http: reqwest::Client,
    token: Option<Mega2Token>,
    pin: MountedPin,
}

impl Mega2TagClient {
    /// Builds the client with the verified pin and the MB-01 timeout.
    pub fn new(server_url: &str, token: Option<Mega2Token>) -> CliResult<Self> {
        Self::with_pin(
            server_url,
            token,
            MountedPin::verified_a1293686(),
            REQUEST_TIMEOUT,
        )
    }

    /// Test seam: caller-provided pin evidence + deadline. An unverified pin
    /// refuses to start (AC-1).
    pub fn with_pin(
        server_url: &str,
        token: Option<Mega2Token>,
        pin: MountedPin,
        timeout: Duration,
    ) -> CliResult<Self> {
        if !pin.is_verified() {
            return Err(CliError::fatal(format!(
                "mega2 tag: DEP-MB-05 pin '{}' is not verified; refusing to start",
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

    /// Whether this client attaches `Authorization` to write requests.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn tag_url(&self, name: &str) -> CliResult<Url> {
        let mut url = self.base.join(TAGS_ROUTE).map_err(|_| {
            CliError::fatal("cannot build the mega2 tag URL")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        url.path_segments_mut()
            .map_err(|_| {
                CliError::fatal("cannot build the mega2 tag URL")
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            })?
            .push(name);
        Ok(url)
    }

    async fn finish<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        operation: &str,
    ) -> CliResult<T> {
        let status = response.status();
        let raw = read_bounded(response).await?;
        parse_common_envelope(status.as_u16(), &raw, operation)
    }

    /// One anonymous `GET /api/v1/tags/list` with all three required keys.
    pub async fn list_tags(&self, page: u64, per_page: u64, path: &str) -> CliResult<TagPage> {
        mega2_diag::run(
            mega2_diag::LIST_TAGS,
            self.list_tags_scoped(page, per_page, path),
        )
        .await
    }

    /// Body of [`Self::list_tags`]; runs inside its diagnostics scope.
    async fn list_tags_scoped(&self, page: u64, per_page: u64, path: &str) -> CliResult<TagPage> {
        validate_pagination(page, per_page)?;
        let path = normalize_path(path)?;
        let mut url = self.base.join(TAGS_LIST_ROUTE).map_err(|_| {
            CliError::fatal("cannot build the mega2 tag list URL")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        url.query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("per_page", &per_page.to_string())
            .append_pair("path", &path);
        // Anonymous: list never carries the write token.
        let response = mega2_diag::send(self.http.get(url), transport_error).await?;
        let page: WirePage<TagInfo> = self.finish(response, "list").await?;
        Ok(TagPage {
            total: page.total,
            items: page.items,
        })
    }

    /// One `POST /api/v1/tags`; any 2xx + `req_result=true` is success.
    pub async fn create_tag(&self, options: &CreateTagOptions<'_>) -> CliResult<TagInfo> {
        mega2_diag::run(mega2_diag::CREATE_TAG, self.create_tag_scoped(options)).await
    }

    /// Body of [`Self::create_tag`]; runs inside its diagnostics scope.
    async fn create_tag_scoped(&self, options: &CreateTagOptions<'_>) -> CliResult<TagInfo> {
        validate_tag_name(options.name)?;
        let url = self.base.join(TAGS_ROUTE).map_err(|_| {
            CliError::fatal("cannot build the mega2 tag URL")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        let mut request = self.http.post(url).json(&create_body(options));
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token.expose()));
        }
        let response = mega2_diag::send(request, transport_error).await?;
        self.finish(response, "create").await
    }

    /// One anonymous `GET /api/v1/tags/{name}` (conflict diagnosis).
    pub async fn get_tag(&self, name: &str, path: &str) -> CliResult<TagInfo> {
        mega2_diag::run(mega2_diag::GET_TAG, self.get_tag_scoped(name, path)).await
    }

    /// Body of [`Self::get_tag`]; runs inside its diagnostics scope.
    async fn get_tag_scoped(&self, name: &str, path: &str) -> CliResult<TagInfo> {
        validate_tag_name(name)?;
        let path = normalize_path(path)?;
        let mut url = self.tag_url(name)?;
        url.query_pairs_mut().append_pair("path", &path);
        let response = mega2_diag::send(self.http.get(url), transport_error).await?;
        self.finish(response, "get").await
    }

    /// One `DELETE /api/v1/tags/{name}`; authorization path = the selector.
    pub async fn delete_tag(&self, name: &str, path: &str) -> CliResult<DeleteTagReceipt> {
        mega2_diag::run(mega2_diag::DELETE_TAG, self.delete_tag_scoped(name, path)).await
    }

    /// Body of [`Self::delete_tag`]; runs inside its diagnostics scope.
    async fn delete_tag_scoped(&self, name: &str, path: &str) -> CliResult<DeleteTagReceipt> {
        validate_tag_name(name)?;
        let path = normalize_path(path)?;
        let mut url = self.tag_url(name)?;
        url.query_pairs_mut().append_pair("path", &path);
        let mut request = self.http.delete(url);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token.expose()));
        }
        let response = mega2_diag::send(request, transport_error).await?;
        let data: WireDeleteTag = self.finish(response, "delete").await?;
        Ok(DeleteTagReceipt {
            deleted_tag: data.deleted_tag,
            message: data.message,
        })
    }
}

fn transport_error(error: reqwest::Error) -> CliError {
    if error.is_timeout() {
        CliError::fatal("mega2 tag request timed out")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else if error.is_connect() {
        CliError::fatal("cannot connect to the mega2 server")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    } else {
        CliError::fatal("mega2 tag request failed")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
    }
}

/// Streams the body with an immediate abort past [`MAX_RESPONSE_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> CliResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(CliError::fatal(format!(
                "mega2 tag response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
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
    fn start_refused_without_mounted_pin() {
        let mut unverified = MountedPin::verified_a1293686();
        unverified.list_is_get_with_required_query = false;
        let err = Mega2TagClient::with_pin(
            "https://mega2.example.com",
            None,
            unverified,
            Duration::from_secs(1),
        )
        .expect_err("unverified pin refuses to start");
        assert_eq!(err.stable_code(), StableErrorCode::Unsupported);
        assert!(err.render().contains("DEP-MB-05"), "{}", err.render());
        assert!(MountedPin::verified_a1293686().is_verified());
    }

    #[test]
    fn tag_names_mirror_the_server_rules() {
        for bad in [
            "",
            "..",
            "a..b",
            "a@{b",
            "a//b",
            "release.lock",
            "has space",
            "tilde~",
            "caret^",
            "colon:",
            "question?",
            "star*",
            "bracket[",
            "back\\slash",
            "nul\0",
            "bell\u{7}",
        ] {
            assert!(
                validate_tag_name(bad).is_err(),
                "expected refusal for {bad:?}"
            );
        }
        assert!(
            validate_tag_name(&"a".repeat(256)).is_err(),
            "over-long refused"
        );
        let max_len = "a".repeat(255);
        for ok in ["v1.0.0", "release-2026", "topic/sub", max_len.as_str()] {
            assert!(validate_tag_name(ok).is_ok(), "expected accept for {ok:?}");
        }
    }

    #[test]
    fn pagination_is_bounded_locally() {
        assert!(validate_pagination(1, 1).is_ok());
        assert!(validate_pagination(MAX_PAGE, MAX_PER_PAGE).is_ok());
        assert!(validate_pagination(0, 10).is_err());
        assert!(validate_pagination(1, 0).is_err(), "per_page=0 refused");
        assert!(validate_pagination(1, MAX_PER_PAGE + 1).is_err());
        assert!(validate_pagination(MAX_PAGE + 1, 10).is_err());
    }

    #[test]
    fn create_body_has_no_tagger_key_and_defaults_the_root_selector() {
        let options = CreateTagOptions {
            name: "v1",
            ..CreateTagOptions::default()
        };
        let body = serde_json::to_value(create_body(&options)).expect("serialize");
        assert_eq!(body["name"], "v1");
        assert_eq!(body["path_context"], "/", "root MVP sends /");
        assert!(body.get("tagger").is_none(), "no third-party key: {body}");
        assert!(body.get("target").is_none());
        assert!(
            body.get("message").is_none(),
            "omitted message = lightweight"
        );

        let annotated = CreateTagOptions {
            name: "v2",
            target: Some("abc"),
            tagger_name: Some("Libra"),
            tagger_email: Some("dev@example.com"),
            message: Some("release"),
            ..CreateTagOptions::default()
        };
        let body = serde_json::to_value(create_body(&annotated)).expect("serialize");
        assert_eq!(body["message"], "release");
        assert_eq!(body["target"], "abc");
        assert_eq!(body["tagger_name"], "Libra");
        assert!(body.get("tagger").is_none());
    }

    #[test]
    fn responses_require_req_result_and_accept_any_2xx() {
        let raw = br#"{"req_result":true,"data":{"name":"v1","tag_id":"t","object_id":"o","object_type":"commit","tagger":"x","message":"","created_at":"now"}}"#;
        for status in [200, 201] {
            let parsed: TagInfo = parse_common_envelope(status, raw, "create").expect("2xx");
            assert_eq!(parsed.name, "v1");
        }
        let failed = br#"{"req_result":false,"data":null}"#;
        assert!(parse_common_envelope::<TagInfo>(200, failed, "create").is_err());
        assert!(parse_common_envelope::<TagInfo>(200, b"not json", "create").is_err());
        assert_eq!(
            parse_common_envelope::<TagInfo>(404, raw, "delete")
                .expect_err("404")
                .stable_code(),
            StableErrorCode::CliInvalidTarget,
            "404 maps to the stable not-found code"
        );
        assert_eq!(
            parse_common_envelope::<TagInfo>(403, raw, "create")
                .expect_err("403")
                .stable_code(),
            StableErrorCode::AuthPermissionDenied
        );
        assert_eq!(
            parse_common_envelope::<TagInfo>(401, raw, "create")
                .expect_err("401")
                .stable_code(),
            StableErrorCode::AuthMissingCredentials
        );
    }
}
