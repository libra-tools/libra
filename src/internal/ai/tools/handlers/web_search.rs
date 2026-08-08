//! Handler for the web_search tool.
//!
//! The tool intentionally returns compact search result metadata rather than
//! fetching arbitrary pages. Page retrieval can be added as a separate tool with
//! its own trust and output-size controls.

use std::{path::Path, time::Duration};

use async_trait::async_trait;
use regex::Regex;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use url::Url;

use super::parse_arguments;
use crate::{
    internal::{
        ai::tools::{
            context::{ToolInvocation, ToolKind, ToolOutput, ToolPayload, WebSearchArgs},
            error::{ToolError, ToolResult},
            registry::ToolHandler,
            spec::ToolSpec,
        },
        config::{LocalIdentityTarget, resolve_env_for_target},
    },
    utils::util::{DATABASE, try_get_storage_path},
};

const BRAVE_SEARCH_API_KEY_ENV: &str = "BRAVE_SEARCH_API_KEY";
const BRAVE_WEB_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const DUCKDUCKGO_HTML_SEARCH_URL: &str = "https://html.duckduckgo.com/html/";
const WEB_SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WEB_SEARCH_RESULTS: usize = 10;
const MAX_SNIPPET_CHARS: usize = 320;
const HTTP_ERROR_PREVIEW_CHARS: usize = 160;

/// Handler for public web search.
///
/// AI user story: let the agent verify current external facts before making
/// time-sensitive claims about APIs, package versions, standards, or vendor
/// behavior. The tool returns compact metadata only; deeper page reads should be
/// a separately reviewed capability.
pub struct WebSearchHandler;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebSearchResult {
    title: String,
    url: String,
    snippet: Option<String>,
}

#[derive(Debug)]
struct RawSearchResult {
    start: usize,
    end: usize,
    title_html: String,
    href: String,
}

#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: Option<String>,
}

#[async_trait]
impl ToolHandler for WebSearchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        ensure_network_allowed(&invocation)?;

        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(ToolError::IncompatiblePayload(
                    "web_search handler only accepts Function payloads".to_string(),
                ));
            }
        };

        let args: WebSearchArgs = parse_arguments(&arguments)?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidArguments(
                "web_search query must not be empty".to_string(),
            ));
        }

        let limit = args.limit.clamp(1, MAX_WEB_SEARCH_RESULTS);
        let results = run_web_search(query, limit, &invocation.working_dir).await?;

        Ok(ToolOutput::success(format_web_search_results(
            query, &results,
        )))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::web_search()
    }
}

async fn run_web_search(
    query: &str,
    limit: usize,
    working_dir: &Path,
) -> ToolResult<Vec<WebSearchResult>> {
    let client = build_web_search_client()?;
    let mut provider_failures = Vec::new();

    if let Some(api_key) = brave_search_api_key(working_dir).await? {
        match fetch_brave_results(&client, query, limit, &api_key).await {
            Ok(results) => return Ok(results),
            Err(error) => provider_failures.push(provider_failure("Brave Search API", error)),
        }
    }

    match fetch_duckduckgo_results(&client, query, limit).await {
        Ok(results) => Ok(results),
        Err(error) => {
            provider_failures.push(provider_failure("DuckDuckGo HTML", error));
            Err(ToolError::ExecutionFailed(format!(
                "web_search failed: {}",
                provider_failures.join("; ")
            )))
        }
    }
}

fn ensure_network_allowed(invocation: &ToolInvocation) -> ToolResult<()> {
    let Some(runtime_context) = invocation.runtime_context.as_ref() else {
        return Ok(());
    };
    let Some(sandbox) = runtime_context.sandbox.as_ref() else {
        return Ok(());
    };

    if sandbox.policy.has_full_network_access() {
        Ok(())
    } else {
        Err(ToolError::ExecutionFailed(
            "web_search requires network access, but the current tool runtime has network access disabled. Enable Network: Allow for the plan or start the TUI with network access allowed."
                .to_string(),
        ))
    }
}

fn build_web_search_client() -> ToolResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(WEB_SEARCH_TIMEOUT)
        .user_agent("libra-code/0.1 (+https://github.com/libra-tools/mega)")
        .build()
        .map_err(|error| {
            ToolError::ExecutionFailed(format!("failed to initialize web search client: {error}"))
        })
}

async fn brave_search_api_key(working_dir: &Path) -> ToolResult<Option<String>> {
    let db_path = try_get_storage_path(Some(working_dir.to_path_buf()))
        .ok()
        .map(|storage| storage.join(DATABASE));
    let local_target = db_path
        .as_deref()
        .map(LocalIdentityTarget::ExplicitDb)
        .unwrap_or(LocalIdentityTarget::None);
    let value = resolve_env_for_target(BRAVE_SEARCH_API_KEY_ENV, local_target)
        .await
        .map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "failed to resolve {BRAVE_SEARCH_API_KEY_ENV} for web_search: {error}"
            ))
        })?;
    Ok(value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

async fn fetch_brave_results(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
    api_key: &str,
) -> ToolResult<Vec<WebSearchResult>> {
    let mut url = Url::parse(BRAVE_WEB_SEARCH_URL).map_err(|error| {
        ToolError::ExecutionFailed(format!("invalid Brave Search API URL: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", &limit.to_string());

    let response = client
        .get(url)
        .header("X-Subscription-Token", api_key)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            ToolError::ExecutionFailed(format!("failed to run Brave Search API request: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        ToolError::ExecutionFailed(format!("failed to read Brave Search API response: {error}"))
    })?;

    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "Brave Search API returned HTTP {}: {}",
            status.as_u16(),
            response_preview(&body)
        )));
    }

    parse_brave_results(&body, limit)
}

fn parse_brave_results(body: &str, limit: usize) -> ToolResult<Vec<WebSearchResult>> {
    let response: BraveSearchResponse = serde_json::from_str(body).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "failed to parse Brave Search API response: {error}"
        ))
    })?;
    let Some(web) = response.web else {
        return Ok(Vec::new());
    };

    Ok(web
        .results
        .into_iter()
        .filter_map(|result| {
            let title = clean_html_text(&result.title);
            let url = result.url.trim().to_string();
            if title.is_empty() || url.is_empty() {
                return None;
            }

            let snippet = result
                .description
                .map(|description| clean_html_text(&description))
                .filter(|description| !description.is_empty())
                .map(|description| truncate_chars(&description, MAX_SNIPPET_CHARS));

            Some(WebSearchResult {
                title,
                url,
                snippet,
            })
        })
        .take(limit)
        .collect())
}

async fn fetch_duckduckgo_results(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> ToolResult<Vec<WebSearchResult>> {
    let html = fetch_duckduckgo_html(client, query).await?;
    parse_duckduckgo_results(&html, limit)
}

async fn fetch_duckduckgo_html(client: &reqwest::Client, query: &str) -> ToolResult<String> {
    let mut url = Url::parse(DUCKDUCKGO_HTML_SEARCH_URL)
        .map_err(|error| ToolError::ExecutionFailed(format!("invalid web search URL: {error}")))?;
    url.query_pairs_mut().append_pair("q", query);

    let response = client.get(url).send().await.map_err(|error| {
        ToolError::ExecutionFailed(format!("failed to run web search request: {error}"))
    })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        ToolError::ExecutionFailed(format!("failed to read web search response: {error}"))
    })?;

    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "web search provider returned HTTP {}: {}",
            status.as_u16(),
            response_preview(&body)
        )));
    }

    Ok(body)
}

fn provider_failure(provider: &str, error: ToolError) -> String {
    match error {
        ToolError::ExecutionFailed(message) => format!("{provider}: {message}"),
        other => format!("{provider}: {other}"),
    }
}

fn response_preview(body: &str) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return truncate_chars(trimmed, HTTP_ERROR_PREVIEW_CHARS);
        }
    }
    "<empty response>".to_string()
}

fn parse_duckduckgo_results(html: &str, limit: usize) -> ToolResult<Vec<WebSearchResult>> {
    let link_re = Regex::new(
        r#"(?is)<a\b[^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .map_err(|error| {
        ToolError::ExecutionFailed(format!("failed to compile web search link parser: {error}"))
    })?;
    let snippet_re =
        Regex::new(r#"(?is)<a\b[^>]*class="[^"]*\bresult__snippet\b[^"]*"[^>]*>(.*?)</a>"#)
            .map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "failed to compile web search snippet parser: {error}"
                ))
            })?;

    let mut raw_results = Vec::new();
    for captures in link_re.captures_iter(html) {
        let Some(full_match) = captures.get(0) else {
            continue;
        };
        let Some(href) = captures.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
        let Some(title_html) = captures.get(2).map(|m| m.as_str().to_string()) else {
            continue;
        };
        raw_results.push(RawSearchResult {
            start: full_match.start(),
            end: full_match.end(),
            title_html,
            href,
        });
    }

    let mut results = Vec::new();
    for (idx, raw) in raw_results.iter().enumerate() {
        if results.len() >= limit {
            break;
        }

        let next_start = raw_results
            .get(idx + 1)
            .map(|next| next.start)
            .unwrap_or(html.len());
        let block = html.get(raw.end..next_start).unwrap_or_default();
        let snippet = snippet_re
            .captures(block)
            .and_then(|captures| captures.get(1))
            .map(|value| clean_html_text(value.as_str()))
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(&value, MAX_SNIPPET_CHARS));

        let title = clean_html_text(&raw.title_html);
        let url = decode_duckduckgo_result_url(&raw.href);
        if title.is_empty() || url.is_empty() {
            continue;
        }

        results.push(WebSearchResult {
            title,
            url,
            snippet,
        });
    }

    Ok(results)
}

fn decode_duckduckgo_result_url(raw: &str) -> String {
    let normalized = if raw.starts_with("//") {
        format!("https:{raw}")
    } else if raw.starts_with('/') {
        format!("https://duckduckgo.com{raw}")
    } else {
        raw.to_string()
    };
    let normalized = normalized.replace("&amp;", "&");

    if let Ok(url) = Url::parse(&normalized)
        && url
            .domain()
            .is_some_and(|domain| domain.ends_with("duckduckgo.com"))
        && url.path().starts_with("/l/")
        && let Some((_, target)) = url.query_pairs().find(|(key, _)| key == "uddg")
    {
        return target.into_owned();
    }

    normalized
}

fn clean_html_text(raw: &str) -> String {
    let without_tags = strip_html_tags(raw);
    collapse_whitespace(&decode_html_entities(&without_tags))
}

fn strip_html_tags(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn decode_html_entities(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(pos) = rest.find('&') {
        output.push_str(&rest[..pos]);
        let after_amp = &rest[pos + 1..];
        if let Some(end) = after_amp.find(';') {
            let entity = &after_amp[..end];
            if let Some(decoded) = decode_html_entity(entity) {
                output.push_str(&decoded);
                rest = &after_amp[end + 1..];
                continue;
            }
        }
        output.push('&');
        rest = after_amp;
    }

    output.push_str(rest);
    output
}

fn decode_html_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        "nbsp" => Some(" ".to_string()),
        _ => decode_numeric_entity(entity),
    }
}

fn decode_numeric_entity(entity: &str) -> Option<String> {
    let value = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        let decimal = entity.strip_prefix('#')?;
        decimal.parse::<u32>().ok()?
    };
    char::from_u32(value).map(|ch| ch.to_string())
}

fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_string();
    }
    let mut truncated = raw
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn format_web_search_results(query: &str, results: &[WebSearchResult]) -> String {
    if results.is_empty() {
        return format!("No web search results found for \"{query}\".");
    }

    let mut lines = vec![format!("Web search results for \"{query}\":")];
    for (idx, result) in results.iter().enumerate() {
        lines.push(format!("{}. {}", idx + 1, result.title));
        lines.push(format!("   URL: {}", result.url));
        if let Some(snippet) = result.snippet.as_deref()
            && !snippet.is_empty()
        {
            lines.push(format!("   Snippet: {snippet}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::internal::ai::sandbox::{
        NetworkAccess, SandboxPermissions, SandboxPolicy, ToolRuntimeContext, ToolSandboxContext,
    };

    #[test]
    fn parses_brave_api_results() {
        let body = r#"
        {
            "web": {
                "results": [
                    {
                        "title": "Announcing <b>Rust</b> 1.85.0 and Rust 2024",
                        "url": "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/",
                        "description": "This stabilizes the <b>2024</b> edition &amp; related changes."
                    },
                    {
                        "title": "",
                        "url": "https://example.com/empty-title",
                        "description": "Skipped because the title is empty."
                    },
                    {
                        "title": "Plain &amp; Simple",
                        "url": "https://example.com/plain",
                        "description": null
                    }
                ]
            }
        }
        "#;

        let results = parse_brave_results(body, 5).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].url,
            "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/"
        );
        assert_eq!(results[0].title, "Announcing Rust 1.85.0 and Rust 2024");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("This stabilizes the 2024 edition & related changes.")
        );
        assert_eq!(results[1].title, "Plain & Simple");
        assert_eq!(results[1].snippet, None);
    }

    #[test]
    fn parses_brave_api_response_without_web_as_empty() {
        let results = parse_brave_results(r#"{"query":{"original":"rust"}}"#, 5).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn parses_duckduckgo_html_results() {
        let html = r#"
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fblog.rust-lang.org%2F2025%2F02%2F20%2FRust-1.85.0%2F&amp;rut=abc">Announcing <b>Rust</b> 1.85.0 and Rust 2024</a>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fblog.rust-lang.org%2F2025%2F02%2F20%2FRust-1.85.0%2F&amp;rut=abc">This stabilizes the <b>2024</b> edition as well.</a>
            <a rel="nofollow" class="result__a" href="https://example.com/plain">Plain &amp; Simple</a>
            <a class="result__snippet" href="https://example.com/plain">A second result &#x27;snippet&#x27;.</a>
        "#;

        let results = parse_duckduckgo_results(html, 5).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].url,
            "https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/"
        );
        assert_eq!(results[0].title, "Announcing Rust 1.85.0 and Rust 2024");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("This stabilizes the 2024 edition as well.")
        );
        assert_eq!(results[1].title, "Plain & Simple");
        assert_eq!(
            results[1].snippet.as_deref(),
            Some("A second result 'snippet'.")
        );
    }

    #[test]
    fn web_search_requires_network_enabled_runtime() {
        let invocation = ToolInvocation::new(
            "call-1",
            "web_search",
            ToolPayload::Function {
                arguments: serde_json::json!({"query": "rust 2024"}).to_string(),
            },
            PathBuf::from("/tmp"),
        )
        .with_runtime_context(ToolRuntimeContext {
            sandbox: Some(ToolSandboxContext {
                policy: SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![PathBuf::from("/tmp")],
                    network_access: NetworkAccess::Denied,
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                    allow_metadata_writes: false,
                },
                permissions: SandboxPermissions::UseDefault,
            }),
            ..ToolRuntimeContext::default()
        });

        let error = ensure_network_allowed(&invocation).unwrap_err();

        assert!(error.to_string().contains("requires network access"));
    }
}
