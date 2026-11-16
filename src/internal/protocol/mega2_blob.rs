//! Bounded text previews from Mega2's storage-only `/api/v1/blob`.
//!
//! An optional token becomes exactly one `Authorization: Bearer` header; without
//! a token the request stays anonymous.

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use url::Url;

use super::{
    mega2_auth::Mega2Token,
    mega2_diag,
    mega2_tree::{MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, normalize_path, validate_server_url},
};
use crate::utils::error::{CliError, CliResult, StableErrorCode};

pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;
pub const BLOB_ROUTE: &str = "/api/v1/blob";

pub struct Mega2BlobClient {
    base: Url,
    http: reqwest::Client,
    token: Option<Mega2Token>,
}

#[derive(Deserialize)]
struct BlobResponse {
    req_result: bool,
    data: Option<String>,
}

impl Mega2BlobClient {
    pub fn new(server: &str) -> CliResult<Self> {
        Self::with_timeout(server, REQUEST_TIMEOUT)
    }

    pub fn new_with_token(server: &str, token: Option<Mega2Token>) -> CliResult<Self> {
        Self::with_timeout_and_token(server, token, REQUEST_TIMEOUT)
    }

    pub fn with_timeout(server: &str, timeout: Duration) -> CliResult<Self> {
        Self::with_timeout_and_token(server, None, timeout)
    }

    pub fn with_timeout_and_token(
        server: &str,
        token: Option<Mega2Token>,
        timeout: Duration,
    ) -> CliResult<Self> {
        let base = validate_server_url(server, token.is_some())?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|_| protocol_error("cannot build the file preview client"))?;
        Ok(Self { base, http, token })
    }

    /// Whether this client will attach an `Authorization` header.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    pub async fn fetch(&self, path: &str, git_ref: Option<&str>) -> CliResult<String> {
        mega2_diag::run(
            mega2_diag::Endpoint {
                method: "GET",
                route: BLOB_ROUTE,
            },
            self.fetch_scoped(path, git_ref),
        )
        .await
    }

    async fn fetch_scoped(&self, path: &str, git_ref: Option<&str>) -> CliResult<String> {
        let path = normalize_path(path)?;
        if path == "/" {
            return Err(
                CliError::fatal("select a file to preview, not the repository root")
                    .with_stable_code(StableErrorCode::CliInvalidTarget),
            );
        }
        let mut url = self
            .base
            .join(BLOB_ROUTE)
            .map_err(|_| protocol_error("cannot build the file preview URL"))?;
        url.query_pairs_mut().append_pair("path", &path);
        if let Some(git_ref) = git_ref.filter(|value| !value.is_empty()) {
            url.query_pairs_mut().append_pair("refs", git_ref);
        }
        let mut request = self.http.get(url);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token.expose()));
        }
        let mut response = mega2_diag::send(request, transport_error).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(CliError::fatal(format!(
                "file preview unavailable (HTTP {status}); redirects are not followed"
            ))
            .with_stable_code(if matches!(status.as_u16(), 401 | 403) {
                StableErrorCode::AuthPermissionDenied
            } else {
                StableErrorCode::NetworkProtocol
            }));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(protocol_error(
                "file preview response exceeds the 1 MiB limit",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(protocol_error(
                    "file preview response exceeds the 1 MiB limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        decode_preview(&body)
    }
}

fn protocol_error(message: &str) -> CliError {
    CliError::fatal(message).with_stable_code(StableErrorCode::NetworkProtocol)
}

fn transport_error(error: reqwest::Error) -> CliError {
    CliError::fatal(if error.is_timeout() {
        "file preview timed out; press r to retry"
    } else {
        "cannot read file preview; check the server connection and file type, then press r to retry"
    })
    .with_stable_code(StableErrorCode::NetworkUnavailable)
}

fn decode_preview(body: &[u8]) -> CliResult<String> {
    let response: BlobResponse = serde_json::from_slice(body)
        .map_err(|_| protocol_error("server returned an invalid file preview response"))?;
    if !response.req_result {
        return Err(protocol_error(
            "server could not read this file for preview",
        ));
    }
    let text = response.data.ok_or_else(|| {
        protocol_error("file content unavailable; the file may be missing or unsupported")
    })?;
    if text.len() > MAX_PREVIEW_BYTES {
        return Err(
            CliError::fatal("file exceeds the 256 KiB text preview limit")
                .with_stable_code(StableErrorCode::Unsupported),
        );
    }
    if text.contains('\0') {
        return Err(CliError::fatal("binary files cannot be shown as text")
            .with_stable_code(StableErrorCode::Unsupported));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[test]
    fn preview_envelope_is_bounded_and_never_echoes_server_errors() {
        for body in [
            br#"{"req_result":false,"data":null,"err_message":"SECRET"}"#.as_slice(),
            b"SECRET",
            br#"{"req_result":true,"data":null}"#,
        ] {
            assert!(
                !decode_preview(body)
                    .unwrap_err()
                    .message()
                    .contains("SECRET")
            );
        }
        let body = serde_json::json!({"req_result":true,"data":"x".repeat(MAX_PREVIEW_BYTES + 1)})
            .to_string();
        assert_eq!(
            decode_preview(body.as_bytes()).unwrap_err().message(),
            "file exceeds the 256 KiB text preview limit"
        );
        assert_eq!(
            decode_preview(br#"{"req_result":true,"data":"a\u0000b"}"#)
                .unwrap_err()
                .message(),
            "binary files cannot be shown as text"
        );
        assert_eq!(
            decode_preview(br#"{"req_result":true,"data":""}"#).unwrap(),
            ""
        );
    }

    fn serve(response: String) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 8192];
            let size = stream.read(&mut request).unwrap();
            let _ = stream.write_all(response.as_bytes());
            String::from_utf8_lossy(&request[..size]).into_owned()
        });
        (base, task)
    }

    #[tokio::test]
    async fn preview_forwards_encoded_path_and_ref_without_credentials() {
        let body = r#"{"req_result":true,"data":"hello\nworld"}"#;
        let (base, task) = serve(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let client = Mega2BlobClient::new(&base).unwrap();
        assert_eq!(
            client
                .fetch("/a b/中.txt", Some("v1&path=/other"))
                .await
                .unwrap(),
            "hello\nworld"
        );
        let request = task.join().unwrap();
        let target = request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        let url = Url::parse(&format!("{base}{target}")).unwrap();
        assert_eq!(url.path(), BLOB_ROUTE);
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [
                ("path".into(), "/a b/中.txt".into()),
                ("refs".into(), "v1&path=/other".into())
            ]
        );
        assert!(!request.to_lowercase().contains("authorization:"));
    }

    #[tokio::test]
    async fn redirects_and_oversized_bodies_fail_closed() {
        for response in [
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/SECRET\r\nContent-Length: 0\r\n\r\n".to_string(),
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_RESPONSE_BYTES + 1),
        ] {
            let (base, task) = serve(response);
            let error = Mega2BlobClient::new(&base).unwrap().fetch("/file", None).await.unwrap_err();
            assert!(!error.message().contains("SECRET"));
            task.join().unwrap();
        }
    }

    #[tokio::test]
    async fn unannounced_oversized_body_is_still_bounded() {
        let (base, task) = serve(format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}",
            "x".repeat(MAX_RESPONSE_BYTES + 1)
        ));
        let error = Mega2BlobClient::new(&base)
            .unwrap()
            .fetch("/file", None)
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "file preview response exceeds the 1 MiB limit"
        );
        task.join().unwrap();
    }

    #[tokio::test]
    async fn local_invalid_paths_make_no_request_and_timeout_is_bounded() {
        let client = Mega2BlobClient::new("http://127.0.0.1:1").unwrap();
        for path in ["/", "relative", "/a/../secret"] {
            assert_eq!(
                client.fetch(path, None).await.unwrap_err().stable_code(),
                StableErrorCode::CliInvalidTarget
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Mega2BlobClient::with_timeout(
            &format!("http://{}", listener.local_addr().unwrap()),
            Duration::from_millis(100),
        )
        .unwrap();
        let task = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(300));
        });
        let error = client.fetch("/file", None).await.unwrap_err();
        assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
        assert_eq!(error.message(), "file preview timed out; press r to retry");
        task.join().unwrap();
    }
}
