//! Machine-readable diagnostics for mega2 HTTP failures (plan-20261001 MN-01).
//!
//! Every request-issuing public method of the four mega2 protocol clients runs
//! its body inside [`run`], and the one request in that body goes through
//! [`send`]. `send` records the outcome of the request — the HTTP status, or the
//! class of a transport failure — in the scope. When the body returns an error
//! after the request went out, `run` adds `method`, `route` and either
//! `http_status` or `transport` to that same error with
//! [`CliError::with_detail`]; errors returned before the request (local
//! validation) pass through untouched. Stable code, message, hints and exit code
//! are never touched.
//!
//! The detail values come only from the [`Endpoint`] constants below and from
//! the status code: never from the response body, `err_message`, the URL, the
//! query or a token. Once released the keys are a public contract
//! (`docs/error-codes.md`, "Command-specific details").

use std::{cell::Cell, future::Future};

use super::{
    mega2_entry::CREATE_ENTRY_ROUTE,
    mega2_mutate::{DELETE_ENTRY_ROUTE, MOVE_ENTRY_ROUTE},
    mega2_tag::{TAGS_LIST_ROUTE, TAGS_ROUTE},
    mega2_tree::TREE_ROUTE,
};
use crate::utils::error::{CliError, CliResult};

/// One mega2 "method + route template" combination. `route` is the template
/// (`/api/v1/tags/{name}`), never a concrete path. Fixed routes reuse the
/// constants the clients join onto the base URL, so a reported route cannot
/// drift from the route actually requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub method: &'static str,
    pub route: &'static str,
}

/// `GET /api/v1/tree` (directory listing).
pub const TREE: Endpoint = Endpoint {
    method: "GET",
    route: TREE_ROUTE,
};
/// `POST /api/v1/create-entry` (create a directory).
pub const CREATE_ENTRY: Endpoint = Endpoint {
    method: "POST",
    route: CREATE_ENTRY_ROUTE,
};
/// `POST /api/v1/delete-entry` (delete a directory).
pub const DELETE_ENTRY: Endpoint = Endpoint {
    method: "POST",
    route: DELETE_ENTRY_ROUTE,
};
/// `POST /api/v1/move-entry` (move or rename a directory).
pub const MOVE_ENTRY: Endpoint = Endpoint {
    method: "POST",
    route: MOVE_ENTRY_ROUTE,
};
/// `GET /api/v1/tags/list` (one page of tags).
pub const LIST_TAGS: Endpoint = Endpoint {
    method: "GET",
    route: TAGS_LIST_ROUTE,
};
/// `POST /api/v1/tags` (create a tag).
pub const CREATE_TAG: Endpoint = Endpoint {
    method: "POST",
    route: TAGS_ROUTE,
};
/// Template of the per-tag route: [`TAGS_ROUTE`] plus one encoded name segment.
const TAG_BY_NAME_ROUTE: &str = "/api/v1/tags/{name}";

/// `GET /api/v1/tags/{name}` (read one tag).
pub const GET_TAG: Endpoint = Endpoint {
    method: "GET",
    route: TAG_BY_NAME_ROUTE,
};
/// `DELETE /api/v1/tags/{name}` (delete a tag).
pub const DELETE_TAG: Endpoint = Endpoint {
    method: "DELETE",
    route: TAG_BY_NAME_ROUTE,
};

/// What the scope's request produced, as far as the diagnostics care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Response headers arrived with this status.
    Status(u16),
    /// The request failed before any status: `timeout`, `connect` or `request`.
    Transport(&'static str),
}

tokio::task_local! {
    static OUTCOME: Cell<Option<Outcome>>;
}

/// Runs one client method body as a diagnostics scope for `endpoint`.
///
/// Returns the body's result unchanged on success. On error, attaches the
/// details described in the module docs when the body sent its request, and
/// returns the original error otherwise.
pub async fn run<T>(endpoint: Endpoint, body: impl Future<Output = CliResult<T>>) -> CliResult<T> {
    OUTCOME
        .scope(Cell::new(None), async move {
            let result = body.await;
            let outcome = OUTCOME.with(Cell::get);
            result.map_err(|error| attach(error, endpoint, outcome))
        })
        .await
}

/// Sends the scope's request and records its outcome.
///
/// `transport_error` is the calling client's own mapping from a transport
/// failure to its stable error, so messages and codes stay exactly as before.
pub async fn send(
    request: reqwest::RequestBuilder,
    transport_error: fn(reqwest::Error) -> CliError,
) -> CliResult<reqwest::Response> {
    match request.send().await {
        Ok(response) => {
            record(Outcome::Status(response.status().as_u16()));
            Ok(response)
        }
        Err(error) => {
            record(Outcome::Transport(transport_class(&error)));
            Err(transport_error(error))
        }
    }
}

/// Mirrors the clients' own `transport_error` branch order.
fn transport_class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else {
        "request"
    }
}

fn record(outcome: Outcome) {
    // Outside a scope there is nothing to annotate; the request still goes out.
    let _ = OUTCOME.try_with(|slot| slot.set(Some(outcome)));
}

fn attach(error: CliError, endpoint: Endpoint, outcome: Option<Outcome>) -> CliError {
    let Some(outcome) = outcome else {
        return error;
    };
    let error = error
        .with_detail("method", endpoint.method)
        .with_detail("route", endpoint.route);
    match outcome {
        Outcome::Status(status) => error.with_detail("http_status", status),
        Outcome::Transport(class) => error.with_detail("transport", class),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        thread,
        time::Duration,
    };

    use serde_json::{Value, json};

    use super::*;
    use crate::utils::error::StableErrorCode;

    /// Serves one connection: reads the request head, then hands the stream to
    /// `reply` (which may write anything, stall, or just drop it).
    fn serve_once(reply: impl FnOnce(TcpStream) + Send + 'static) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                reply(stream);
            }
        });
        addr
    }

    fn http(timeout: Duration) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .expect("test client")
    }

    /// Stands in for a client's own transport mapping.
    fn transport_error(_: reqwest::Error) -> CliError {
        CliError::fatal("t")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
            .with_hint("h")
    }

    fn details(value: Value) -> BTreeMap<String, Value> {
        value
            .as_object()
            .expect("details object")
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// Runs one scope against `url` whose only request goes through [`send`];
    /// a received status becomes `status_error`.
    async fn status_scope(url: String, status_error: CliError) -> CliResult<()> {
        run(TREE, async move {
            let _response = send(http(Duration::from_secs(5)).get(url), transport_error).await?;
            Err(status_error)
        })
        .await
    }

    fn reply_500(mut stream: TcpStream) {
        let _ = stream.write_all(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    }

    // G1
    #[tokio::test]
    async fn run_attaches_status_to_scope_error() {
        let addr = serve_once(reply_500);
        let err = status_scope(
            format!("http://{addr}/"),
            CliError::fatal("m").with_stable_code(StableErrorCode::NetworkProtocol),
        )
        .await
        .expect_err("scope error");
        assert_eq!(
            err.details(),
            &details(json!({"method": "GET", "route": "/api/v1/tree", "http_status": 500}))
        );
    }

    // G2
    #[tokio::test]
    async fn run_classifies_timeout() {
        // Accept the connection and keep it open without ever replying.
        let addr = serve_once(|stream| {
            thread::sleep(Duration::from_secs(3));
            drop(stream);
        });
        let err = run(TREE, async move {
            send(
                http(Duration::from_millis(200)).get(format!("http://{addr}/")),
                transport_error,
            )
            .await
            .map(|_| ())
        })
        .await
        .expect_err("timeout");
        assert_eq!(
            err.details(),
            &details(json!({"method": "GET", "route": "/api/v1/tree", "transport": "timeout"}))
        );
    }

    /// Returns an address that refuses connections (a port bound then freed).
    fn refused_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let addr = listener.local_addr().expect("probe addr");
        drop(listener);
        addr
    }

    // G3
    #[tokio::test]
    async fn run_classifies_connect() {
        let addr = refused_addr();
        let err = run(TREE, async move {
            send(
                http(Duration::from_secs(5)).get(format!("http://{addr}/")),
                transport_error,
            )
            .await
            .map(|_| ())
        })
        .await
        .expect_err("connect refused");
        assert_eq!(
            err.details(),
            &details(json!({"method": "GET", "route": "/api/v1/tree", "transport": "connect"}))
        );
    }

    // G4
    #[tokio::test]
    async fn run_classifies_malformed_response_as_request() {
        let addr = serve_once(|mut stream| {
            let _ = stream.write_all(b"this is not HTTP\r\n\r\n");
        });
        let err = run(TREE, async move {
            send(
                http(Duration::from_secs(5)).get(format!("http://{addr}/")),
                transport_error,
            )
            .await
            .map(|_| ())
        })
        .await
        .expect_err("malformed response");
        assert_eq!(
            err.details(),
            &details(json!({"method": "GET", "route": "/api/v1/tree", "transport": "request"}))
        );
    }

    // G27
    #[tokio::test]
    async fn run_returns_original_scope_error_plus_details() {
        let addr = serve_once(reply_500);
        let original = CliError::fatal("m")
            .with_stable_code(StableErrorCode::NetworkProtocol)
            .with_hint("h1")
            .with_hint("h2");
        let err = status_scope(format!("http://{addr}/"), original.clone())
            .await
            .expect_err("scope error");
        assert_eq!(
            err,
            original
                .with_detail("method", "GET")
                .with_detail("route", "/api/v1/tree")
                .with_detail("http_status", 500)
        );
    }

    // G28
    #[tokio::test]
    async fn run_returns_original_transport_error_plus_details() {
        let addr = refused_addr();
        let err = run(TREE, async move {
            send(
                http(Duration::from_secs(5)).get(format!("http://{addr}/")),
                transport_error,
            )
            .await
            .map(|_| ())
        })
        .await
        .expect_err("connect refused");
        let original = CliError::fatal("t")
            .with_stable_code(StableErrorCode::NetworkUnavailable)
            .with_hint("h");
        assert_eq!(
            err,
            original
                .with_detail("method", "GET")
                .with_detail("route", "/api/v1/tree")
                .with_detail("transport", "connect")
        );
    }

    /// The per-tag template is the collection route plus one `{name}` segment.
    #[test]
    fn tag_by_name_route_extends_the_collection_route() {
        assert_eq!(TAG_BY_NAME_ROUTE, format!("{TAGS_ROUTE}/{{name}}"));
    }

    // G29
    #[tokio::test]
    async fn run_leaves_errors_before_send_untouched() {
        let original = CliError::fatal("local")
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("fix the input");
        let err = run(TREE, async { Err::<(), _>(original.clone()) })
            .await
            .expect_err("local error");
        assert_eq!(err, original);
    }
}
