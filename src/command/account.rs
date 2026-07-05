use std::{net::IpAddr, process::Command, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use reqwest::Client;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

use crate::{
    internal::account::{self, AccountSession},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

const DEFAULT_HOST: &str = "https://libra.tools";
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Parser, Debug)]
pub struct LoginArgs {
    #[arg(long, default_value = DEFAULT_HOST)]
    pub host: String,
    #[arg(long)]
    pub no_browser: bool,
}

#[derive(Parser, Debug)]
pub struct WhoamiArgs {
    #[arg(long, default_value = DEFAULT_HOST)]
    pub host: String,
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Parser, Debug)]
pub struct LogoutArgs {
    #[arg(long, default_value = DEFAULT_HOST)]
    pub host: String,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub local_only: bool,
}

#[derive(Debug, Deserialize)]
struct ExchangeResponse {
    session_token: String,
    username: String,
    user_id: String,
    github_id: String,
    host: String,
    issued_at: String,
    expires_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WhoamiResponse {
    username: String,
    user_id: String,
    github_id: String,
    host: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
    message: Option<String>,
}

fn host_is_loopback(host: &str) -> bool {
    let ip_host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    ip_host.eq_ignore_ascii_case("localhost")
        || ip_host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn origin_is_loopback(origin: &str) -> bool {
    Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host_is_loopback(&host))
}

fn account_http_client(host: &str) -> CliResult<Client> {
    let builder = if origin_is_loopback(host) {
        Client::builder().no_proxy()
    } else {
        Client::builder()
    };
    builder
        .build()
        .map_err(|error| CliError::network(format!("failed to build account HTTP client: {error}")))
}

fn normalize_host(input: &str) -> CliResult<String> {
    let raw = input.trim();
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&with_scheme).map_err(|error| {
        CliError::command_usage(format!("invalid --host: {error}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| CliError::command_usage("--host is missing a hostname"))?;
    let loopback = host_is_loopback(host);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(CliError::command_usage(
            "--host must use https, except http loopback hosts for local testing",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            CliError::command_usage("--host must not include credentials")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(CliError::command_usage(
            "--host must be an origin only, for example https://libra.tools",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    Ok(url.origin().ascii_serialization())
}

fn random_bytes(len: usize) -> CliResult<Vec<u8>> {
    let rng = SystemRandom::new();
    let mut bytes = vec![0; len];
    rng.fill(&mut bytes).map_err(|_| {
        CliError::fatal("failed to generate secure random bytes")
            .with_stable_code(StableErrorCode::InternalInvariant)
    })?;
    Ok(bytes)
}

fn random_base64url(len: usize) -> CliResult<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(len)?))
}

fn user_code() -> CliResult<String> {
    let rng = SystemRandom::new();
    let limit = (u8::MAX as usize / USER_CODE_ALPHABET.len()) * USER_CODE_ALPHABET.len();
    let mut out = Vec::with_capacity(8);
    while out.len() < 8 {
        let mut b = [0u8; 1];
        rng.fill(&mut b).map_err(|_| {
            CliError::fatal("failed to generate secure random bytes")
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?;
        if (b[0] as usize) < limit {
            out.push(USER_CODE_ALPHABET[(b[0] as usize) % USER_CODE_ALPHABET.len()]);
        }
    }
    String::from_utf8(out).map_err(|e| CliError::internal(e.to_string()))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn browser_open(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", vec![url]);

    Command::new(cmd.0)
        .args(cmd.1)
        .spawn()
        .map(|_| true)
        .unwrap_or(false)
}

async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> CliResult<String> {
    let accept = tokio::time::timeout(LOGIN_TIMEOUT, listener.accept())
        .await
        .map_err(|_| {
            CliError::network("timed out waiting for browser authorization")
                .with_hint("run `libra login` again; remote browser/SSH sessions are not supported")
        })?;
    let (mut stream, _) = accept
        .map_err(|e| CliError::network(format!("failed to accept loopback callback: {e}")))?;
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| CliError::network(format!("failed to read loopback callback: {e}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| CliError::network("loopback callback was malformed"))?;
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|e| CliError::network(format!("loopback callback URL was malformed: {e}")))?;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, v)| v);
    if state.as_deref() != Some(expected_state) {
        write_loopback_response(&mut stream, false).await;
        return Err(CliError::fatal("state mismatch in browser callback")
            .with_stable_code(StableErrorCode::AuthPermissionDenied));
    }
    if let Some(error) = url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .map(|(_, v)| v)
    {
        write_loopback_response(&mut stream, false).await;
        return Err(CliError::fatal(format!("authorization failed: {error}"))
            .with_stable_code(StableErrorCode::AuthPermissionDenied));
    }
    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| CliError::network("loopback callback did not include an auth code"))?;
    write_loopback_response(&mut stream, true).await;
    Ok(code)
}

async fn write_loopback_response(stream: &mut tokio::net::TcpStream, ok: bool) {
    let body = loopback_response_html(ok);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.as_bytes().len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn loopback_response_html(ok: bool) -> &'static str {
    if ok {
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Libra CLI Login Complete</title>
</head>
<body>
<main>
<h1>Libra CLI login complete</h1>
<p>You can close this tab.</p>
</main>
</body>
</html>"#
    } else {
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Libra CLI Login Failed</title>
</head>
<body>
<main>
<h1>Libra CLI login failed</h1>
<p>Return to your terminal.</p>
</main>
</body>
</html>"#
    }
}

async fn error_text(response: reqwest::Response) -> String {
    let status = response.status();
    match response.json::<ErrorResponse>().await {
        Ok(body) => body.message.unwrap_or(body.error),
        Err(_) => format!("server returned {status}"),
    }
}

pub async fn login(args: LoginArgs, output: &OutputConfig) -> CliResult<()> {
    let host = normalize_host(&args.host)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| CliError::network(format!("failed to bind loopback callback: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| CliError::network(format!("failed to inspect loopback callback: {e}")))?
        .port();
    let code = user_code()?;
    let state = random_base64url(24)?;
    let verifier = random_base64url(32)?;
    let challenge = pkce_challenge(&verifier);
    let mut login_url = Url::parse(&format!("{host}/api/cli/login"))
        .map_err(|e| CliError::internal(format!("failed to construct account login URL: {e}")))?;
    login_url
        .query_pairs_mut()
        .append_pair("user_code", &code)
        .append_pair("callback_port", &port.to_string())
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("client", "libra-cli");

    if args.no_browser || !browser_open(login_url.as_str()) {
        println!("Open this URL in your browser:\n{}", login_url.as_str());
    }
    if !output.quiet {
        println!("Verification code: {code}");
    }

    let auth_code = wait_for_callback(listener, &state).await?;
    let client = account_http_client(&host)?;
    let response = client
        .post(format!("{host}/api/cli/exchange"))
        .json(&serde_json::json!({ "code": auth_code, "code_verifier": verifier }))
        .send()
        .await
        .map_err(|e| CliError::network(format!("failed to exchange auth code: {e}")))?;
    if !response.status().is_success() {
        return Err(CliError::fatal(format!(
            "failed to complete CLI login: {}",
            error_text(response).await
        ))
        .with_stable_code(StableErrorCode::AuthPermissionDenied));
    }
    let exchanged = response.json::<ExchangeResponse>().await.map_err(|e| {
        CliError::fatal(format!("server returned an invalid login response: {e}"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    })?;
    let session = AccountSession {
        host: exchanged.host.clone(),
        username: exchanged.username,
        user_id: exchanged.user_id,
        github_id: exchanged.github_id,
        session_token: exchanged.session_token,
        issued_at: exchanged.issued_at,
        expires_at: exchanged.expires_at,
    };
    account::store_session(&session)
        .await
        .map_err(|e| CliError::fatal(format!("failed to store account session: {e}")))?;
    if output.is_json() {
        return emit_json_data("login", &session, output);
    }
    if !output.quiet {
        println!("Logged in to {} as {}", session.host, session.username);
    }
    Ok(())
}

pub async fn whoami(args: WhoamiArgs, output: &OutputConfig) -> CliResult<()> {
    let host = normalize_host(&args.host)?;
    let Some(session) = account::load_session(&host)
        .await
        .map_err(|e| CliError::fatal(format!("failed to read account session: {e}")))?
    else {
        return Err(CliError::fatal(format!("not logged in to {host}"))
            .with_stable_code(StableErrorCode::AuthMissingCredentials)
            .with_hint("run `libra login`"));
    };
    let client = account_http_client(&host)?;
    let response = client
        .get(format!("{host}/api/cli/whoami"))
        .bearer_auth(&session.session_token)
        .send()
        .await
        .map_err(|e| CliError::network(format!("failed to verify account session: {e}")))?;
    if !response.status().is_success() {
        return Err(CliError::fatal(format!(
            "account session is not valid: {}",
            error_text(response).await
        ))
        .with_stable_code(StableErrorCode::AuthPermissionDenied));
    }
    let body = response.json::<WhoamiResponse>().await.map_err(|e| {
        CliError::fatal(format!("server returned an invalid whoami response: {e}"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    })?;
    if output.is_json() {
        return emit_json_data("whoami", &body, output);
    }
    if !output.quiet {
        println!("{} ({}) on {}", body.username, body.github_id, body.host);
        println!("expires {}", body.expires_at);
    }
    Ok(())
}

pub async fn logout(args: LogoutArgs, output: &OutputConfig) -> CliResult<()> {
    let host = normalize_host(&args.host)?;
    let session = account::load_session(&host)
        .await
        .map_err(|e| CliError::fatal(format!("failed to read account session: {e}")))?;
    if !args.local_only
        && let Some(session) = &session
    {
        let response = account_http_client(&host)?
            .post(format!("{host}/api/cli/logout"))
            .bearer_auth(&session.session_token)
            .json(&serde_json::json!({ "all": args.all }))
            .send()
            .await
            .map_err(|e| CliError::network(format!("failed to revoke server session: {e}")))?;
        if !response.status().is_success() {
            return Err(CliError::fatal(format!(
                "failed to revoke server session: {}",
                error_text(response).await
            ))
            .with_stable_code(StableErrorCode::AuthPermissionDenied)
            .with_hint("use `libra logout --local-only` only if you accept leaving the server session valid until expiry"));
        }
    }
    let removed = if args.all {
        account::remove_all_sessions().await
    } else {
        account::remove_session(&host)
            .await
            .map(|removed| usize::from(removed))
    }
    .map_err(|e| CliError::fatal(format!("failed to remove local account session: {e}")))?;
    if output.is_json() {
        return emit_json_data("logout", &serde_json::json!({ "removed": removed }), output);
    }
    if !output.quiet {
        println!("removed {removed} local account session(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_accepts_https_origin_and_loopback_http() {
        assert_eq!(
            normalize_host("libra.tools").unwrap(),
            "https://libra.tools"
        );
        assert_eq!(
            normalize_host("http://127.0.0.1:7001").unwrap(),
            "http://127.0.0.1:7001"
        );
        assert_eq!(
            normalize_host("http://[::1]:7001").unwrap(),
            "http://[::1]:7001"
        );
    }

    #[test]
    fn normalize_host_rejects_paths_and_non_loopback_http() {
        assert!(normalize_host("https://libra.tools/path").is_err());
        assert!(normalize_host("http://example.com").is_err());
    }

    #[test]
    fn origin_loopback_detection_matches_local_test_hosts() {
        assert!(origin_is_loopback("http://localhost:7001"));
        assert!(origin_is_loopback("http://127.0.0.1:7001"));
        assert!(origin_is_loopback("http://[::1]:7001"));
        assert!(!origin_is_loopback("https://libra.tools"));
        assert!(!origin_is_loopback("http://example.com"));
    }

    #[test]
    fn loopback_callback_page_sets_browser_title() {
        assert!(loopback_response_html(true).contains("<title>Libra CLI Login Complete</title>"));
        assert!(loopback_response_html(false).contains("<title>Libra CLI Login Failed</title>"));
    }

    #[test]
    fn generated_user_code_uses_approved_alphabet() {
        let code = user_code().unwrap();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|byte| USER_CODE_ALPHABET.contains(&byte)));
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        let challenge = pkce_challenge("test-verifier");
        assert_eq!(challenge.len(), 43);
        assert!(
            challenge
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' })
        );
    }
}
