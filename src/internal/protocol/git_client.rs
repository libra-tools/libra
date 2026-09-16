//! Git protocol (git://) client that connects over TCP, advertises refs, and streams pack data.

use std::{io::Error as IoError, time::Duration};

use bytes::{Bytes, BytesMut};
use futures_util::stream::{self, StreamExt};
use git_internal::errors::GitError;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use url::Url;

use super::{
    DiscoveryResult, FetchStream, ProtocolClient, generate_upload_pack_content,
    parse_discovered_references,
};
use crate::git_protocol::{
    PktLineError, ServiceType, add_pkt_line_string, decode_pkt_line_header, is_pkt_line_io_error,
    pkt_frame_payload_len, pkt_line_read_error,
};

const DEFAULT_GIT_PORT: u16 = 9418;

pub struct GitClient {
    host: String,
    port: u16,
    repo_path: String,
    /// TCP connect timeout — bounds `open_stream` so a black-holed host cannot
    /// hang a `git://` fetch forever.
    connect_timeout: Duration,
    /// Idle read timeout — bounds each read so a stalled peer (no bytes for this
    /// long) is treated as a dead connection rather than an infinite wait.
    idle_timeout: Duration,
    /// First-byte timeout — bounds the wait from sending the `want` list to the
    /// first byte of the response (`NAK` / pack header), so a server that
    /// accepts the negotiation but never starts streaming is caught sooner than
    /// the (longer) idle timeout would.
    first_byte_timeout: Duration,
}

/// Default `git://` connect timeout when nothing overrides it.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default `git://` idle (per-read) timeout when nothing overrides it.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Default `git://` first-byte timeout when nothing overrides it.
const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);

impl ProtocolClient for GitClient {
    fn from_url(url: &Url) -> Self {
        let host = url.host_str().unwrap_or_default().to_string();
        let port = url.port().unwrap_or(DEFAULT_GIT_PORT);
        let mut repo_path = url.path().to_string();
        if repo_path.ends_with('/') && repo_path.len() > 1 {
            repo_path.pop();
        }
        Self {
            host,
            port,
            repo_path,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
        }
    }
}

impl GitClient {
    fn build_service_request(&self, service: ServiceType) -> Bytes {
        let mut buf = BytesMut::new();
        let request = format!("{service} {}\0host={}\0", self.repo_path, self.host);
        add_pkt_line_string(&mut buf, request);
        buf.freeze()
    }

    /// Override the connect and idle timeouts (resolved from
    /// `LIBRA_FETCH_*_MS` / `fetch.<remote>.*Timeout` at the fetch layer).
    pub fn with_network_timeouts(
        mut self,
        connect_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        self.connect_timeout = connect_timeout;
        self.idle_timeout = idle_timeout;
        self
    }

    /// Override the first-byte timeout (the wait for the first response byte
    /// after the `want` list is sent).
    pub fn with_first_byte_timeout(mut self, first_byte_timeout: Duration) -> Self {
        self.first_byte_timeout = first_byte_timeout;
        self
    }

    async fn open_stream(&self) -> Result<TcpStream, IoError> {
        match tokio::time::timeout(
            self.connect_timeout,
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(IoError::other(format!(
                "git:// connect to {}:{} timed out after {}s",
                self.host,
                self.port,
                self.connect_timeout.as_secs()
            ))),
        }
    }

    /// Read exactly `buf.len()` bytes, failing if the peer goes idle for longer
    /// than `idle_timeout` (rather than blocking forever on a stalled socket).
    async fn read_exact_idle<R: AsyncRead + Unpin>(
        &self,
        stream: &mut R,
        buf: &mut [u8],
    ) -> Result<(), IoError> {
        match tokio::time::timeout(self.idle_timeout, stream.read_exact(buf)).await {
            Ok(result) => result.map(|_| ()),
            Err(_) => Err(IoError::other(format!(
                "git:// connection idle for more than {}s",
                self.idle_timeout.as_secs()
            ))),
        }
    }

    /// Write all of `data` (then flush), bounded by `idle_timeout`, so a peer
    /// that accepts the connection but stops reading — leaving our socket send
    /// buffer full — cannot hang the fetch on an unbounded write.
    async fn write_all_idle(&self, stream: &mut TcpStream, data: &[u8]) -> Result<(), IoError> {
        let write = async {
            stream.write_all(data).await?;
            stream.flush().await
        };
        match tokio::time::timeout(self.idle_timeout, write).await {
            Ok(result) => result,
            Err(_) => Err(IoError::other(format!(
                "git:// write stalled for more than {}s (peer not reading)",
                self.idle_timeout.as_secs()
            ))),
        }
    }

    async fn read_advertisement<R: AsyncRead + Unpin>(
        &self,
        stream: &mut R,
    ) -> Result<Bytes, IoError> {
        let mut buf = BytesMut::new();
        loop {
            let mut len_buf = [0u8; 4];
            self.read_exact_idle(stream, &mut len_buf)
                .await
                .map_err(|error| pkt_line_read_error(error, PktLineError::TruncatedHeader))?;
            let len = decode_pkt_line_header(&len_buf)
                .map_err(|error| IoError::new(std::io::ErrorKind::InvalidData, error))?
                as usize;
            buf.extend_from_slice(&len_buf);
            if len == 0 {
                break;
            }
            let payload_len = pkt_frame_payload_len(len as u32).map_err(|error| {
                IoError::new(std::io::ErrorKind::InvalidData, PktLineError::from(error))
            })?;
            let mut data = vec![0u8; payload_len];
            self.read_exact_idle(stream, &mut data)
                .await
                .map_err(|error| pkt_line_read_error(error, PktLineError::TruncatedPayload))?;
            buf.extend_from_slice(&data);
        }
        Ok(buf.freeze())
    }

    pub async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        let mut stream = self
            .open_stream()
            .await
            .map_err(|e| GitError::NetworkError(format!("Failed to connect: {e}")))?;
        let request = self.build_service_request(service);
        self.write_all_idle(&mut stream, &request)
            .await
            .map_err(|e| GitError::NetworkError(format!("Failed to send request: {e}")))?;
        let response = self
            .read_advertisement(&mut stream)
            .await
            .map_err(|error| {
                if is_pkt_line_io_error(&error) {
                    GitError::NetworkError(error.to_string())
                } else {
                    GitError::NetworkError(format!("Failed to read response: {error}"))
                }
            })?;
        parse_discovered_references(response, service)
    }

    pub async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        let mut stream = self.open_stream().await?;
        let request = self.build_service_request(ServiceType::UploadPack);
        self.write_all_idle(&mut stream, &request).await?;
        self.read_advertisement(&mut stream).await?;

        let body = generate_upload_pack_content(have, want, shallow, depth);
        self.write_all_idle(&mut stream, &body).await?;

        // Read the pack with a per-read IDLE bound (the timer resets whenever
        // bytes arrive), NOT a single total deadline — so a large but healthy
        // pack over a slow link is never cut off, while a peer that stalls for
        // longer than `idle_timeout` is treated as dead. The FIRST read uses the
        // (typically shorter) first-byte timeout: the wait from sending the
        // `want` list to the first `NAK` / pack byte.
        let mut response = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        let mut first_read = true;
        loop {
            let bound = if first_read {
                self.first_byte_timeout
            } else {
                self.idle_timeout
            };
            let read = match tokio::time::timeout(bound, stream.read(&mut chunk)).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(IoError::other(if first_read {
                        format!(
                            "git:// no response within {}s of sending the request (first byte)",
                            self.first_byte_timeout.as_secs()
                        )
                    } else {
                        format!(
                            "git:// pack read idle for more than {}s",
                            self.idle_timeout.as_secs()
                        )
                    }));
                }
            };
            first_read = false;
            if read == 0 {
                break; // EOF — the peer closed the stream.
            }
            response.extend_from_slice(&chunk[..read]);
        }
        Ok(stream::once(async move { Ok(Bytes::from(response)) }).boxed())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    struct Pkt13TcpFixture {
        listener: std::sync::Arc<tokio::net::TcpListener>,
        task: tokio::task::JoinHandle<Vec<u8>>,
        url: String,
    }
    impl Drop for Pkt13TcpFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Pkt13TcpFixture {
        async fn new(wire: Option<&[u8]>) -> Self {
            let listener =
                std::sync::Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
            let url = format!("git://{}/repo", listener.local_addr().unwrap());
            let acceptor = listener.clone();
            let wire = wire.map(<[u8]>::to_vec);
            let task = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(10), async move {
                    let (mut socket, _) = acceptor.accept().await.unwrap();
                    let mut header = [0; 4];
                    socket.read_exact(&mut header).await.unwrap();
                    let length = crate::git_protocol::decode_pkt_line_header(&header).unwrap();
                    assert!((4..=1024).contains(&length), "bounded service request");
                    let mut request = vec![0; pkt_frame_payload_len(length).unwrap()];
                    socket.read_exact(&mut request).await.unwrap();
                    if let Some(wire) = wire {
                        // A peer may reject the header and close before this
                        // fixture finishes writing or half-closing its socket.
                        // Command assertions still require the exact typed error.
                        let allow_rejected_peer_close = |result: std::io::Result<()>| {
                            match result {
                                Ok(()) => {}
                                Err(error) if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::ConnectionReset
                                        | std::io::ErrorKind::BrokenPipe
                                        | std::io::ErrorKind::NotConnected
                                ) => {}
                                Err(error) => panic!("TCP fixture IO failed: {error}"),
                            }
                        };
                        allow_rejected_peer_close(socket.write_all(&wire).await);
                        allow_rejected_peer_close(socket.shutdown().await);
                        let mut unexpected = [0; 1];
                        match socket.read(&mut unexpected).await {
                            Ok(0) => {}
                            Err(error) if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::NotConnected
                            ) => {}
                            other => panic!("client must stop before sending negotiation or pack bytes: {other:?}"),
                        }
                    } else {
                        // A real idle peer, not a fabricated error string.
                        std::future::pending::<()>().await;
                    }
                    request
                })
                .await
                .expect("TCP fixture must remain bounded")
            });
            Self {
                listener,
                task,
                url,
            }
        }
        async fn verify(&mut self, service: &str) {
            let request = tokio::time::timeout(Duration::from_secs(12), &mut self.task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                request,
                format!("{service} /repo\0host=127.0.0.1\0").as_bytes()
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), self.listener.accept())
                    .await
                    .is_err(),
                "no extra connection may be queued"
            );
        }
    }

    #[tokio::test]
    async fn pkt_line_header_git_client_header_marker() {
        for (wire, expected) in [
            (b"+004".as_slice(), PktLineError::InvalidHexHeader),
            (b"SECR", PktLineError::InvalidHexHeader),
            (b"\xff000", PktLineError::InvalidHeaderEncoding),
        ] {
            assert_typed_frame_error(read_frame_fixture(wire).await.unwrap_err(), expected);
        }
    }

    #[tokio::test]
    async fn pkt_line_header_git_discovery_wrapper_passthrough() {
        for (wire, reason) in [
            (b"SECR".as_slice(), PktLineError::InvalidHexHeader),
            (b"000", PktLineError::TruncatedHeader),
            (b"0005", PktLineError::TruncatedPayload),
        ] {
            let mut fixture = Pkt13TcpFixture::new(Some(wire)).await;
            let client = GitClient::from_url(&Url::parse(&fixture.url).unwrap())
                .with_network_timeouts(Duration::from_secs(1), Duration::from_secs(1));
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                client.discovery_reference(ServiceType::UploadPack),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                matches!(&error, GitError::NetworkError(detail) if detail == &reason.to_string()),
                "{error:?}"
            );
            fixture.verify("git-upload-pack").await;
        }
        let fixture = Pkt13TcpFixture::new(None).await;
        let client = GitClient::from_url(&Url::parse(&fixture.url).unwrap())
            .with_network_timeouts(Duration::from_secs(1), Duration::from_millis(200));
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.discovery_reference(ServiceType::UploadPack),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(&error, GitError::NetworkError(detail) if detail.starts_with("Failed to read response: ") && detail.contains("connection idle")),
            "{error:?}"
        );
        // Dropping the fixture aborts its idle server and releases the listener.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(env, cwd, hash_kind)]
    async fn pkt_line_header_git_frame_errors_end_to_end_net_002() {
        use clap::Parser;

        use crate::{
            command::{clone, fetch, ls_remote, pull, push},
            git_protocol::PktFrameError,
            internal::{branch::Branch, config::ConfigKv},
            utils::{
                error::StableErrorCode,
                output::OutputConfig,
                test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
            },
        };
        let _storage = ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let cases = [
            (
                b"0001".as_slice(),
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (
                b"0002",
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (
                b"0003",
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (b"", PktLineError::TruncatedHeader),
            (b"0", PktLineError::TruncatedHeader),
            (b"00", PktLineError::TruncatedHeader),
            (b"000", PktLineError::TruncatedHeader),
            (b"0005", PktLineError::TruncatedPayload),
            (
                b"0040PKT13_REMOTE_SECRET_8af32",
                PktLineError::TruncatedPayload,
            ),
            (b"ffffabc", PktLineError::TruncatedPayload),
            (
                b"SECRPKT13_REMOTE_SECRET_8af32",
                PktLineError::InvalidHexHeader,
            ),
            (b"+004", PktLineError::InvalidHexHeader),
            (b" 004", PktLineError::InvalidHexHeader),
            (b"\xff000", PktLineError::InvalidHeaderEncoding),
        ];
        let mut checked = 0;
        for (wire, reason) in cases {
            for command in ["ls-remote", "fetch", "clone", "pull", "push"] {
                let repo = tempfile::tempdir().unwrap();
                setup_with_new_libra_in(repo.path()).await;
                let _cwd = ChangeDirGuard::new(repo.path());
                let mut fixture = Pkt13TcpFixture::new(Some(wire)).await;
                ConfigKv::set("remote.origin.url", &fixture.url, false)
                    .await
                    .unwrap();
                let tracking = "refs/remotes/origin/main";
                let oid = "1111111111111111111111111111111111111111";
                if command == "push" {
                    Branch::update_branch(tracking, oid, Some("origin"))
                        .await
                        .unwrap();
                }
                let target = repo.path().join("clone-target");
                let output = OutputConfig::default();
                let error = tokio::time::timeout(Duration::from_secs(30), async {
                    match command {
                        "ls-remote" => ls_remote::execute_safe(
                            ls_remote::LsRemoteArgs::try_parse_from(["ls-remote", "origin"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "fetch" => fetch::execute_safe(
                            fetch::FetchArgs::try_parse_from(["fetch", "origin"]).unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "clone" => clone::execute_safe(
                            clone::CloneArgs::try_parse_from([
                                "clone",
                                fixture.url.as_str(),
                                target.to_str().unwrap(),
                            ])
                            .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "pull" => pull::execute_safe(
                            pull::PullArgs::try_parse_from(["pull", "--ff-only", "origin", "main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "push" => push::execute_safe(
                            push::PushArgs::try_parse_from(["push", "origin", ":refs/heads/main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        _ => unreachable!(),
                    }
                })
                .await
                .expect("malformed TCP command must terminate");
                assert_eq!(
                    error.stable_code(),
                    StableErrorCode::NetworkProtocol,
                    "{command}: {error:?}"
                );
                assert_eq!(error.stable_code().as_str(), "LBR-NET-002");
                assert_eq!(error.stable_code().exit_code().as_i32(), 128);
                assert!(
                    error.message().contains(&reason.to_string()),
                    "{command}: {error:?}"
                );
                let hint = if command == "push" {
                    "check the remote Git service or proxy response and retry"
                } else {
                    "check that the remote serves Git data and that a proxy has not altered the response"
                };
                assert_eq!(
                    error
                        .hints()
                        .iter()
                        .map(|item| item.as_str())
                        .collect::<Vec<_>>(),
                    [hint],
                    "{command}: {error:?}"
                );
                for rendered in [
                    error.render(),
                    error.render_report(),
                    error.render_json().to_string(),
                ] {
                    for forbidden in ["PKT13_REMOTE_SECRET", "SECR", "+004", "�"] {
                        assert!(!rendered.contains(forbidden), "{command}: {rendered}");
                    }
                }
                fixture
                    .verify(if command == "push" {
                        "git-receive-pack"
                    } else {
                        "git-upload-pack"
                    })
                    .await;
                let actual = Branch::find_branch_result(tracking, Some("origin"))
                    .await
                    .unwrap();
                if command == "push" {
                    assert_eq!(actual.unwrap().commit.to_string(), oid);
                } else {
                    assert!(
                        actual.is_none(),
                        "failed discovery must not create a tracking ref"
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 70);
    }

    use super::*;

    pub(crate) async fn read_test_stream<R: AsyncRead + Unpin>(
        stream: &mut R,
        idle: Duration,
    ) -> Result<Bytes, IoError> {
        let client = GitClient::from_url(&Url::parse("git://fixture.invalid/repo").unwrap())
            .with_network_timeouts(Duration::from_secs(1), idle);
        client.read_advertisement(stream).await
    }

    pub(crate) async fn read_frame_fixture(mut input: &[u8]) -> Result<Bytes, IoError> {
        read_test_stream(&mut input, Duration::from_secs(1)).await
    }

    async fn assert_tcp_fetch_advertisement_error(input: &[u8], expected: PktLineError) {
        use crate::{
            command::{clone::CloneError, fetch::FetchError, pull::PullError},
            utils::error::{CliError, StableErrorCode},
        };

        // The object-fetch advertisement uses a second TCP connection and
        // returns the real reader error without discovery's string wrapper.
        // Exercise that public transport method before the command conversions.
        for command in ["fetch", "clone", "pull"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let input = input.to_vec();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = [0; 4];
                socket.read_exact(&mut header).await.unwrap();
                let len = usize::from_str_radix(std::str::from_utf8(&header).unwrap(), 16).unwrap();
                assert!((4..=1024).contains(&len));
                let mut request = vec![0; pkt_frame_payload_len(len as u32).unwrap()];
                socket.read_exact(&mut request).await.unwrap();
                socket.write_all(&input).await.unwrap();
                socket.shutdown().await.unwrap();
                request
            });
            struct AbortServer(tokio::task::JoinHandle<Vec<u8>>);
            impl Drop for AbortServer {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }
            let mut server = AbortServer(server);
            let client =
                GitClient::from_url(&Url::parse(&format!("git://{address}/repo")).unwrap())
                    .with_network_timeouts(Duration::from_secs(1), Duration::from_secs(1));
            let source = tokio::time::timeout(
                Duration::from_secs(5),
                client.fetch_objects(&[], &[], &[], None),
            )
            .await
            .expect("TCP fetch advertisement bounded")
            .err()
            .expect("bad advertisement must fail before upload-pack request");
            let request = tokio::time::timeout(Duration::from_secs(5), &mut server.0)
                .await
                .expect("TCP fixture bounded")
                .expect("TCP fixture succeeded");
            assert_eq!(
                request.as_slice(),
                b"git-upload-pack /repo\0host=127.0.0.1\0"
            );
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(
                source
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<PktLineError>()),
                Some(&expected)
            );
            let source = FetchError::FetchObjects {
                remote: "fixture".to_string(),
                source,
            };
            let cli: CliError = match command {
                "fetch" => source.into(),
                "clone" => CloneError::FetchFailed { source }.into(),
                "pull" => PullError::Fetch(source).into(),
                _ => unreachable!(),
            };
            assert_eq!(cli.stable_code(), StableErrorCode::NetworkProtocol);
            assert_eq!(cli.stable_code().as_str(), "LBR-NET-002");
            assert_eq!(cli.stable_code().exit_code().as_i32(), 128);
            assert!(cli.message().contains(&expected.to_string()));
            assert_eq!(
                cli.hints().iter().map(|h| h.as_str()).collect::<Vec<_>>(),
                [
                    "check that the remote serves Git data and that a proxy has not altered the response"
                ]
            );
        }
    }

    #[tokio::test]
    async fn pkt_line_client_git_rejects_len_below_four() {
        for input in [b"0001", b"0002", b"0003"] {
            assert_typed_frame_error(
                read_frame_fixture(input).await.unwrap_err(),
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                ),
            );
            assert_tcp_fetch_advertisement_error(
                input,
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                ),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn pkt_line_client_git_flush_regression() {
        assert_eq!(
            read_frame_fixture(b"0000zzzz").await.unwrap(),
            b"0000".as_slice()
        );
    }

    #[tokio::test]
    async fn pkt_line_client_git_len4_regression() {
        let input = b"00040005x0000";
        assert_eq!(read_frame_fixture(input).await.unwrap(), input.as_slice());
    }

    #[tokio::test]
    async fn pkt_line_client_git_upper_bound_regression() {
        for header in [b"ffff", b"FFFF"] {
            let mut input = header.to_vec();
            input.extend_from_slice(&vec![0xff; 0xffff - 4]);
            input.extend_from_slice(b"0000");
            assert_eq!(read_frame_fixture(&input).await.unwrap(), input.as_slice());
        }
    }

    pub(crate) fn assert_typed_frame_error(error: IoError, expected: PktLineError) {
        use crate::{
            command::fetch::FetchError,
            utils::error::{CliError, StableErrorCode},
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<PktLineError>()),
            Some(&expected)
        );
        assert_eq!(error.to_string(), expected.to_string());
        assert!(
            error
                .to_string()
                .starts_with(crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX)
        );
        let cli = CliError::from(FetchError::PacketRead { source: error });
        assert_eq!(cli.stable_code(), StableErrorCode::NetworkProtocol);
        assert_eq!(cli.stable_code().as_str(), "LBR-NET-002");
        assert_eq!(cli.stable_code().exit_code().as_i32(), 128);
        assert!(cli.hints().is_empty());
    }

    struct ResetReader;
    impl AsyncRead for ResetReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(IoError::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset fixture",
            )))
        }
    }

    fn assert_transport_error(error: IoError, reason: &str) {
        use crate::{
            command::fetch::FetchError,
            utils::error::{CliError, StableErrorCode},
        };
        assert!(
            !error
                .to_string()
                .starts_with(crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX)
        );
        assert!(error.to_string().contains(reason));
        let cli = CliError::from(FetchError::PacketRead { source: error });
        assert_eq!(cli.stable_code(), StableErrorCode::NetworkUnavailable);
        assert_eq!(cli.stable_code().as_str(), "LBR-NET-001");
        assert_eq!(
            cli.hints().iter().map(|h| h.as_str()).collect::<Vec<_>>(),
            ["check network connectivity and retry"]
        );
    }

    #[tokio::test]
    async fn pkt_line_client_truncated_payload_eof_net_002() {
        use crate::internal::protocol::ssh_client::tests as ssh;
        for input in [b"0005".as_slice(), b"0008abc", b"ffffabc"] {
            assert_typed_frame_error(
                read_frame_fixture(input).await.unwrap_err(),
                PktLineError::TruncatedPayload,
            );
            assert_typed_frame_error(
                ssh::read_frame_fixture(input).await.unwrap_err(),
                PktLineError::TruncatedPayload,
            );
            assert_tcp_fetch_advertisement_error(input, PktLineError::TruncatedPayload).await;
        }
        // Ordinary IO retains its reason and existing per-client context/kind.
        let mut reset = ResetReader;
        let error = read_test_stream(&mut reset, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        assert_transport_error(error, "connection reset fixture");
        let error = ssh::read_test_stream(&mut reset, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_transport_error(error, "SSH read failed: connection reset fixture");
    }

    #[tokio::test]
    async fn pkt_line_client_partial_header_eof_net_002() {
        use crate::internal::protocol::ssh_client::tests as ssh;
        for input in [
            b"".as_slice(),
            b"0",
            b"00",
            b"000",
            b"0005x",
            b"0005x0",
            b"0005x00",
            b"0005x000",
        ] {
            assert_typed_frame_error(
                read_frame_fixture(input).await.unwrap_err(),
                PktLineError::TruncatedHeader,
            );
            assert_typed_frame_error(
                ssh::read_frame_fixture(input).await.unwrap_err(),
                PktLineError::TruncatedHeader,
            );
            assert_tcp_fetch_advertisement_error(input, PktLineError::TruncatedHeader).await;
        }
        // Keep the duplex peer open to exercise actual timeout, not EOF.
        let (_writer, mut reader) = tokio::io::duplex(1);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            read_test_stream(&mut reader, Duration::from_millis(10)),
        )
        .await
        .expect("bounded git reader")
        .unwrap_err();
        assert_transport_error(error, "idle");
        let (_writer, mut reader) = tokio::io::duplex(1);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            ssh::read_test_stream(&mut reader, Duration::from_millis(10)),
        )
        .await
        .expect("bounded ssh reader")
        .unwrap_err();
        assert_transport_error(error, "timed out");
    }

    #[test]
    fn from_url_sets_default_timeouts() {
        let url = Url::parse("git://example.com/repo.git").unwrap();
        let client = GitClient::from_url(&url);
        assert_eq!(client.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(client.idle_timeout, DEFAULT_IDLE_TIMEOUT);
    }

    #[test]
    fn with_network_timeouts_overrides_defaults() {
        let url = Url::parse("git://example.com/repo.git").unwrap();
        let client = GitClient::from_url(&url)
            .with_network_timeouts(Duration::from_secs(3), Duration::from_secs(7));
        assert_eq!(client.connect_timeout, Duration::from_secs(3));
        assert_eq!(client.idle_timeout, Duration::from_secs(7));
    }

    #[test]
    fn first_byte_timeout_defaults_and_overrides() {
        let url = Url::parse("git://example.com/repo.git").unwrap();
        let client = GitClient::from_url(&url);
        assert_eq!(client.first_byte_timeout, DEFAULT_FIRST_BYTE_TIMEOUT);
        let client = client.with_first_byte_timeout(Duration::from_secs(4));
        assert_eq!(client.first_byte_timeout, Duration::from_secs(4));
    }

    #[tokio::test]
    async fn discovery_returns_promptly_on_a_refused_connection() {
        // Bind then drop a listener so the port is free but unused; a connect
        // there is refused promptly. Deterministic on any network (unlike a
        // black-holed address), and it exercises that the connect result
        // propagates as an error instead of hanging.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let url = Url::parse(&format!("git://127.0.0.1:{port}/repo.git")).unwrap();
        let client = GitClient::from_url(&url)
            .with_network_timeouts(Duration::from_secs(5), Duration::from_secs(60));
        let started = tokio::time::Instant::now();
        let result = client.discovery_reference(ServiceType::UploadPack).await;
        assert!(result.is_err(), "a refused connect should fail");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a refused connect must return promptly, took {:?}",
            started.elapsed()
        );
    }
}
