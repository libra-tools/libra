//! Typed Mega2 TraceRepo transport (LTR-11).
//!
//! Ordinary code remotes are never rewritten. A scoped SSH URL is derived
//! with the reserved `traces` login. Admission decisions are pure: they
//! do not open a socket, do not turn a non-fast-forward rejection into a
//! force, and do not create an epoch.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub const CAPABILITY: &str = "libra-traces-v1";
pub const SCOPE_CAPABILITY_PREFIX: &str = "libra-traces-scope=";
pub const SERVER_CAPABILITY_PREFIX: &str = "libra-traces-server=";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    Sha1,
    Sha256,
}

impl HashKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByPathLocator {
    pub target_path: String,
    pub source_id: String,
    pub epoch: String,
    pub hash_kind: HashKind,
}

/// What a caller may do before any socket is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportPlan {
    pub url_path: String,
    pub ssh_login: Option<String>,
    pub http_registration: bool,
    pub force: bool,
    pub opens_new_epoch: bool,
    pub falls_back_to_code_push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Push(TransportPlan),
    Reject {
        message: String,
        plan: TransportPlan,
    },
}

pub fn encode_locator_segment(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn normalize_target_path(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("target path is empty".to_string());
    }
    let mut normalized = if path == "/" {
        "/".to_string()
    } else {
        path.trim_end_matches('/').to_string()
    };
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if normalized
        .split('/')
        .skip(1)
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!(
            "target path {path:?} is not canonical; keep spaces, drop '.' and '..', and use one leading slash"
        ));
    }
    Ok(normalized)
}

pub fn by_path_git_path(locator: &ByPathLocator) -> Result<String, String> {
    let target = normalize_target_path(&locator.target_path)?;
    let source = locator.source_id.as_bytes();
    if source.is_empty() || locator.epoch.is_empty() {
        return Err("source and epoch are required before a traces URL is built".to_string());
    }
    Ok(format!(
        "/.traces/by-path/{}/self/{}/{}/{}.git",
        encode_locator_segment(target.as_bytes()),
        encode_locator_segment(source),
        locator.epoch,
        locator.hash_kind.as_str()
    ))
}

pub fn by_scope_git_path(stream_id: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(stream_id)
        .map_err(|_| format!("stream id {stream_id:?} is not a UUID"))?;
    Ok(format!("/.traces/by-scope/{stream_id}.git"))
}

/// Derive an SSH URL whose login is `traces`. The input remote is not modified.
pub fn derive_scoped_ssh_uri(remote_url: &str) -> Result<String, String> {
    let rest = remote_url
        .strip_prefix("ssh://")
        .ok_or_else(|| format!("scoped traces SSH requires an ssh:// URL, got {remote_url:?}"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if host.is_empty() || host.contains('@') {
        return Err(format!(
            "cannot derive a traces SSH host from {remote_url:?}"
        ));
    }
    let path = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    Ok(format!("ssh://traces@{host}{path}"))
}

/// HTTP(S) URL of a traces locator on the origin of `remote_url`.
///
/// Credentials, query and fragment of the stored remote are dropped; the
/// stored remote itself is not modified.
pub fn traces_http_url(remote_url: &str, locator_path: &str) -> Result<String, String> {
    let mut url = url::Url::parse(remote_url)
        .map_err(|_| format!("remote URL {remote_url:?} is not an HTTP(S) Mega2 endpoint"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(format!(
            "remote URL {remote_url:?} is not an HTTP(S) Mega2 endpoint"
        ));
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path(locator_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

/// SSH URL of a traces locator with the reserved `traces` login.
pub fn traces_ssh_url(remote_url: &str, locator_path: &str) -> Result<String, String> {
    let derived = derive_scoped_ssh_uri(remote_url)?;
    let rest = derived
        .strip_prefix("ssh://")
        .ok_or_else(|| format!("derived SSH URL {derived:?} has no ssh scheme"))?;
    let authority = rest.split_once('/').map(|(head, _)| head).unwrap_or(rest);
    Ok(format!("ssh://{authority}{locator_path}"))
}

/// Canonical project path named by a code remote URL, such as `/project/foo`
/// for `https://host/project/foo.git`. Returns `None` when the URL has no
/// usable path (a bare origin, a percent-encoded path, or a traces locator);
/// the caller then requires an explicit `--target`.
pub fn target_from_remote_url(remote_url: &str) -> Option<String> {
    let url = url::Url::parse(remote_url).ok()?;
    let path = url.path().trim_end_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() || path.contains('%') || path.split('/').any(|part| part == ".traces") {
        return None;
    }
    normalize_target_path(path).ok()
}

pub fn advertisement_has_traces_v1(advertisement: &str) -> bool {
    advertisement
        .split(|character: char| character.is_whitespace() || character == '\0')
        .any(|token| token == CAPABILITY || token.starts_with(&format!("{CAPABILITY}=")))
}

pub fn advertised_scope(advertisement: &str) -> Option<String> {
    advertisement.split_whitespace().find_map(|token| {
        token
            .strip_prefix(SCOPE_CAPABILITY_PREFIX)
            .map(str::to_string)
    })
}

pub fn advertised_server_id(advertisement: &str) -> Option<String> {
    advertisement.split_whitespace().find_map(|token| {
        token
            .strip_prefix(SERVER_CAPABILITY_PREFIX)
            .map(str::to_string)
    })
}

fn idle_plan(url_path: &str) -> TransportPlan {
    TransportPlan {
        url_path: url_path.to_string(),
        ssh_login: None,
        http_registration: false,
        force: false,
        opens_new_epoch: false,
        falls_back_to_code_push: false,
    }
}

/// Decide a Mega2 traces push before any network I/O.
pub fn admit_scoped_push(
    mega2_mode: bool,
    force_rewrite: bool,
    ssh: bool,
    advertisement: &str,
    non_fast_forward: bool,
    locator_path: &str,
) -> Admission {
    let mut plan = idle_plan(locator_path);
    let code_boundary = [
        crate::internal::protocol::git_client::ordinary_code_transport_allows(locator_path).err(),
        crate::internal::protocol::https_client::ordinary_code_transport_allows(locator_path).err(),
        crate::internal::protocol::ssh_client::ordinary_code_transport_allows(locator_path).err(),
    ];
    if locator_path.contains("/.traces/") && code_boundary.iter().any(Option::is_none) {
        return Admission::Reject {
            message: "an ordinary code transport failed to refuse this traces locator".to_string(),
            plan,
        };
    }
    if ssh {
        plan.ssh_login = Some("traces".to_string());
        plan.http_registration = false;
    }
    if mega2_mode && force_rewrite {
        return Admission::Reject {
            message: "Mega2 traces mode refuses force-rewrite before any network attempt. \
                      Retry without --force-rewrite. A non-fast-forward result is not converted \
                      into a force, does not open a new epoch, and does not fall back to a code push"
                .to_string(),
            plan,
        };
    }
    if !advertisement_has_traces_v1(advertisement) {
        return Admission::Reject {
            message: "remote did not advertise libra-traces-v1; refusing to write. This is not \
                      a code push"
                .to_string(),
            plan,
        };
    }
    if non_fast_forward {
        return Admission::Reject {
            message: "remote rejected a non-fast-forward traces update. The client will not \
                      force the ref, will not create an epoch, and will not push the code remote"
                .to_string(),
            plan,
        };
    }
    Admission::Push(plan)
}

pub fn admit_before_network(mega2_mode: bool, force_rewrite: bool) -> Result<(), String> {
    match admit_scoped_push(mega2_mode, force_rewrite, false, CAPABILITY, false, "") {
        Admission::Reject { message, .. } => Err(message),
        Admission::Push(_) => Ok(()),
    }
}

/// Ordinary code transports must not carry a traces locator.
/// A Mega2 traces Git URL. Ordinary `clone` must not download it.
pub fn is_traces_locator_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.contains("/.traces/")
}

/// Mega2 code-view endpoint: `http(s)` or `ssh` under `/project/`, not a traces locator.
pub fn is_mega2_code_view_url(url: &str) -> bool {
    if is_traces_locator_url(url) {
        return false;
    }
    let (scheme, _) = url.split_once("://").unwrap_or(("", url));
    if !matches!(scheme, "https" | "http" | "ssh") {
        return false;
    }
    url.contains("/project/") && url.contains(".git")
}

/// Mirror refspec. Mega2 code view stores branch tips only, so `refs/libra/traces` is not fetched.
pub fn mirror_fetch_refspec(url: &str) -> &'static str {
    if is_mega2_code_view_url(url) {
        "+refs/heads/*:refs/heads/*"
    } else {
        "+refs/*:refs/*"
    }
}

/// Code-view fetches keep branch and tag tips and drop the traces ref.
pub fn code_view_keeps_ref(name: &str) -> bool {
    if name == "refs/libra/traces" || name.starts_with("refs/libra/traces/") {
        return false;
    }
    name.starts_with("refs/heads/") || name.starts_with("refs/tags/") || name == "HEAD"
}

pub fn reject_traces_locator_on_code_transport(url: &str) -> Result<(), String> {
    if url.contains("/.traces/") {
        Err(format!(
            "refusing to use an ordinary code transport for traces URL {url:?}; use the typed TraceRepo client"
        ))
    } else {
        Ok(())
    }
}

pub fn propagate_server_rejection(stable_code: &str) -> Result<(), String> {
    match stable_code {
        "unauthenticated"
        | "unknown_scope"
        | "invalid_parameter"
        | "invalid_cursor"
        | "unsupported_schema"
        | "invalid_capture"
        | "object_integrity"
        | "response_budget"
        | "query_timeout"
        | "search_unavailable"
        | "trace_history_in_code" => Err(format!("traces remote rejected the push: {stable_code}")),
        other => Err(format!(
            "traces remote returned an unknown rejection {other:?}; not treated as success"
        )),
    }
}

pub const PROTOCOL_ID: &str = "libra-traces/v1";
pub const RESPONSE_BYTES_MAX: usize = 1_048_576;
pub const METADATA_LIMIT_DEFAULT: u64 = 50;
pub const METADATA_LIMIT_MAX: u64 = 200;
pub const SEARCH_LIMIT_DEFAULT: u64 = 20;
pub const SEARCH_LIMIT_MAX: u64 = 100;
pub const TEXT_BYTES_DEFAULT: u64 = 16_384;
pub const TEXT_BYTES_MAX: u64 = 131_072;
pub const QUERY_BYTES_MIN: usize = 1;
pub const QUERY_BYTES_MAX: usize = 256;

pub fn same_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Keep `Authorization` only when a redirect stays on the same origin.
pub fn bearer_after_redirect(
    current: &url::Url,
    next: &url::Url,
    authorization: Option<&str>,
) -> Option<String> {
    if same_origin(current, next) {
        authorization.map(str::to_string)
    } else {
        None
    }
}

pub fn take_bounded(bytes: &[u8], limit: usize) -> Result<&[u8], String> {
    if bytes.len() > limit {
        Err(format!(
            "traces response is {} bytes, over the {limit} byte limit",
            bytes.len()
        ))
    } else {
        Ok(bytes)
    }
}

pub fn validate_search_query(query: &str) -> Result<(), String> {
    let length = query.len();
    if length < QUERY_BYTES_MIN {
        Err("search query must be at least 1 UTF-8 byte; the request was not sent".to_string())
    } else if length > QUERY_BYTES_MAX {
        Err(format!(
            "search query is {length} UTF-8 bytes; the maximum is {QUERY_BYTES_MAX} and the request was not sent"
        ))
    } else {
        Ok(())
    }
}

pub fn bounded_count(
    name: &str,
    value: Option<u64>,
    default: u64,
    max: u64,
) -> Result<u64, String> {
    let chosen = value.unwrap_or(default);
    if chosen == 0 || chosen > max {
        Err(format!("{name} must be from 1 to {max}"))
    } else {
        Ok(chosen)
    }
}

pub fn parse_rfc3339_unix_seconds(value: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.timestamp())
        .map_err(|_| format!("time bound {value:?} must be an RFC3339 timestamp"))
}

pub fn validate_time_window(since: Option<i64>, until: Option<i64>) -> Result<(), String> {
    if let (Some(since), Some(until)) = (since, until)
        && since > until
    {
        return Err(
            "since must be earlier than until; the interval is half-open and the request was not sent"
                .to_string(),
        );
    }
    Ok(())
}

/// Normalize an RFC3339 timestamp to UTC seconds, written with a `Z` suffix.
pub fn normalize_rfc3339_utc(value: &str) -> Result<String, String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| format!("time bound {value:?} must be an RFC3339 timestamp"))?;
    Ok(parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Accept a full 40- or 64-character hexadecimal code OID and leave the text unchanged.
pub fn validate_code_commit(value: &str) -> Result<(), String> {
    let hexadecimal = value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if hexadecimal && (value.len() == 40 || value.len() == 64) {
        Ok(())
    } else {
        Err(format!(
            "code commit {value:?} must be 40 or 64 hexadecimal characters; the request was not sent"
        ))
    }
}

/// Distinguish a missing or non-boolean readiness field from `search_ready=false`.
pub fn search_ready_gate(
    field: Option<&serde_json::Value>,
    reason: Option<&str>,
) -> Result<(), String> {
    match field {
        None => Err("search_ready is missing; the readiness check was not accepted".to_string()),
        Some(value) => match value.as_bool() {
            Some(ready) => require_search_ready(ready, reason),
            None => Err(
                "search_ready must be a boolean; the readiness check was not accepted".to_string(),
            ),
        },
    }
}

pub fn require_search_ready(ready: bool, reason: Option<&str>) -> Result<(), String> {
    if ready {
        Ok(())
    } else {
        Err(format!(
            "search is not ready ({})",
            reason.unwrap_or("search_unavailable")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::protocol::{git_client, https_client, ssh_client};

    #[test]
    fn scoped_git_transport_matrix() {
        let locator = ByPathLocator {
            target_path: "/project/foo bar/".to_string(),
            source_id: "opaque-legacy-value".to_string(),
            epoch: "11111111-1111-4111-8111-111111111111".to_string(),
            hash_kind: HashKind::Sha256,
        };
        let path = by_path_git_path(&locator).expect("by-path");
        assert!(path.starts_with("/.traces/by-path/"));
        assert!(path.contains("/self/"));
        assert!(path.ends_with("/11111111-1111-4111-8111-111111111111/sha256.git"));
        assert!(!path.contains(' '));
        assert!(path.contains(&encode_locator_segment("/project/foo bar".as_bytes())));
        let scope = by_scope_git_path("22222222-2222-4222-8222-222222222222").expect("by-scope");
        assert_eq!(
            scope,
            "/.traces/by-scope/22222222-2222-4222-8222-222222222222.git"
        );
        assert!(by_scope_git_path("not-a-uuid").is_err());
        assert!(normalize_target_path("/project/../x").is_err());

        let derived =
            derive_scoped_ssh_uri("ssh://git@example.test:2222/project/foo.git").expect("ssh");
        assert_eq!(derived, "ssh://traces@example.test:2222/project/foo.git");
        assert_eq!(
            derive_scoped_ssh_uri("ssh://git@example.test:2222/project/foo.git").unwrap(),
            derived,
            "derivation does not depend on mutating the original remote"
        );
        assert!(derive_scoped_ssh_uri("https://example.test/foo.git").is_err());

        let early = admit_before_network(true, true).expect_err("force before network");
        assert!(early.contains("before any network"));
        assert!(admit_before_network(false, true).is_ok());

        let missing = admit_scoped_push(true, false, true, "agent report-status", false, &path);
        match missing {
            Admission::Reject { message, plan } => {
                assert!(message.contains(CAPABILITY));
                assert_eq!(plan.ssh_login.as_deref(), Some("traces"));
                assert!(!plan.http_registration);
                assert!(!plan.force);
                assert!(!plan.opens_new_epoch);
                assert!(!plan.falls_back_to_code_push);
            }
            Admission::Push(_) => panic!("missing capability must reject"),
        }

        let rejected = admit_scoped_push(true, false, false, "libra-traces-v1 agent", true, &path);
        match rejected {
            Admission::Reject { plan, .. } => {
                assert!(!plan.force);
                assert!(!plan.opens_new_epoch);
                assert!(!plan.falls_back_to_code_push);
                assert!(!plan.http_registration);
            }
            Admission::Push(_) => panic!("non-ff must reject"),
        }

        let allowed = admit_scoped_push(
            true,
            false,
            true,
            "side-band-64k libra-traces-v1",
            false,
            &path,
        );
        match allowed {
            Admission::Push(plan) => {
                assert_eq!(plan.ssh_login.as_deref(), Some("traces"));
                assert!(
                    !plan.http_registration,
                    "the first SSH push does not register over HTTP"
                );
                assert_eq!(plan.url_path, path);
            }
            Admission::Reject { message, .. } => {
                panic!("capable ssh push should be allowed: {message}")
            }
        }

        let scope_id =
            advertised_scope("libra-traces-v1 libra-traces-scope=abc libra-traces-server=server")
                .expect("scope");
        assert_eq!(scope_id, "abc");
        assert_eq!(
            advertised_server_id("libra-traces-server=server").as_deref(),
            Some("server")
        );
        assert!(
            propagate_server_rejection("unknown_scope")
                .unwrap_err()
                .contains("unknown_scope")
        );
        assert!(propagate_server_rejection("trace_history_in_code").is_err());

        https_client::ordinary_code_transport_allows("https://example.test/project/foo.git")
            .expect("code https");
        assert!(
            https_client::ordinary_code_transport_allows(&format!("https://example.test{path}"))
                .is_err()
        );
        git_client::ordinary_code_transport_allows("git://example.test/project/foo.git")
            .expect("code git");
        ssh_client::ordinary_code_transport_allows("ssh://git@example.test/project/foo.git")
            .expect("code ssh");
        assert!(
            ssh_client::ordinary_code_transport_allows(&format!("ssh://traces@example.test{path}"))
                .is_err()
        );
    }

    #[test]
    fn auth_redirect_and_bounded_response_matrix() {
        let origin = url::Url::parse("http://127.0.0.1:9/api/v1/traces/capabilities").unwrap();
        let same = url::Url::parse("http://127.0.0.1:9/api/v1/traces/next").unwrap();
        let cross = url::Url::parse("https://evil.example/api/v1/traces/next").unwrap();
        assert_eq!(
            bearer_after_redirect(&origin, &same, Some("Bearer secret")).as_deref(),
            Some("Bearer secret")
        );
        assert!(bearer_after_redirect(&origin, &cross, Some("Bearer secret")).is_none());
        assert!(take_bounded(&vec![1; RESPONSE_BYTES_MAX], RESPONSE_BYTES_MAX).is_ok());
        assert!(take_bounded(&vec![1; RESPONSE_BYTES_MAX + 1], RESPONSE_BYTES_MAX).is_err());
    }

    #[test]
    fn bounded_body_and_offsets() {
        assert!(validate_search_query("a").is_ok());
        assert!(validate_search_query(&"a".repeat(256)).is_ok());
        assert!(validate_search_query("").is_err());
        assert!(validate_search_query(&"a".repeat(257)).is_err());
        assert!(validate_search_query("你").is_ok());
        let since = parse_rfc3339_unix_seconds("2026-11-02T00:00:00Z").unwrap();
        let until = parse_rfc3339_unix_seconds("2026-11-02T01:00:00Z").unwrap();
        assert!(validate_time_window(Some(since), Some(until)).is_ok());
        assert!(validate_time_window(Some(until), Some(since)).is_err());
        assert!(
            validate_time_window(Some(since), Some(since)).is_ok(),
            "since equal to until is an empty interval, not a local rejection"
        );
        assert_eq!(
            normalize_rfc3339_utc("2026-11-02T08:00:00+08:00").unwrap(),
            "2026-11-02T00:00:00Z"
        );
        assert!(validate_code_commit(&"ab".repeat(20)).is_ok());
        assert!(validate_code_commit(&"ab".repeat(32)).is_ok());
        assert!(validate_code_commit("abcd").is_err());
        assert!(
            search_ready_gate(None, None)
                .unwrap_err()
                .contains("missing")
        );
        assert!(
            search_ready_gate(Some(&serde_json::json!("yes")), None)
                .unwrap_err()
                .contains("boolean")
        );
        assert_eq!(
            bounded_count("limit", None, TEXT_BYTES_DEFAULT, TEXT_BYTES_MAX).unwrap(),
            TEXT_BYTES_DEFAULT
        );
        assert!(
            bounded_count(
                "max_bytes",
                Some(TEXT_BYTES_MAX + 1),
                TEXT_BYTES_DEFAULT,
                TEXT_BYTES_MAX
            )
            .is_err()
        );
        assert!(require_search_ready(false, Some("extension_missing")).is_err());
        assert!(require_search_ready(true, None).is_ok());
    }
}
