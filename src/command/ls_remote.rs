//! Implements `ls-remote` to list refs advertised by a remote repository.

use std::{io::Write, path::Path};

use clap::Parser;
use git_internal::errors::GitError;
use serde::Serialize;
use url::Url;

use crate::{
    command::fetch::{RemoteClient, is_pkt_line_io_error, resolve_remote_default_branch},
    git_protocol::{PKT_LINE_PROTOCOL_ERROR_PREFIX, ServiceType::UploadPack},
    internal::{
        config::ConfigKv,
        protocol::{DiscRef, ssh_client::is_ssh_spec},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[path = "ls_remote_filter.rs"]
mod ls_remote_filter;
#[path = "ls_remote_redaction.rs"]
mod ls_remote_redaction;
#[cfg(test)]
#[path = "ls_remote_tests.rs"]
mod ls_remote_tests;

use ls_remote_filter::{compile_patterns, include_reference, sort_entries};
use ls_remote_redaction::{
    sanitize_discovery_error, sanitize_remote_error_reason, visible_remote_display,
    visible_remote_url,
};

const LS_REMOTE_EXAMPLES: &str = "\
EXAMPLES:
    libra ls-remote origin                          List all refs on a configured remote
    libra ls-remote https://example.com/repo.git    List all refs on a remote URL (no remote setup)
    libra ls-remote --get-url origin                Resolve a remote URL without contacting it
    libra ls-remote --heads origin main             List only branch heads matching `main`
    libra ls-remote --exit-code origin main         Exit 2 when no refs match
    libra ls-remote --symref origin                 Show symbolic-ref targets (e.g. HEAD)
    libra --json ls-remote --tags origin            Structured JSON output for agents (tags only)";

#[derive(Parser, Debug)]
#[command(after_help = LS_REMOTE_EXAMPLES)]
pub struct LsRemoteArgs {
    /// Show only branch refs (refs/heads/)
    #[clap(long)]
    pub heads: bool,

    /// Show only tag refs (refs/tags/)
    #[clap(long, short = 't')]
    pub tags: bool,

    /// Do not show HEAD or peeled tag refs (refs ending in ^{})
    #[clap(long)]
    pub refs: bool,

    /// Expand the remote URL and exit without contacting the remote
    #[clap(long)]
    pub get_url: bool,

    /// Exit with status 2 when no refs match
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Sort refs by key: refname, -refname, version:refname, or -version:refname
    #[clap(long, value_name = "KEY")]
    pub sort: Option<String>,

    /// Show the targets of symbolic refs advertised by the remote (e.g.
    /// `ref: refs/heads/main\tHEAD`)
    #[clap(long)]
    pub symref: bool,

    /// Remote name, URL, or local repository path
    pub repository: String,

    /// Optional ref patterns. Plain names match full refs or path components.
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LsRemoteEntry {
    hash: String,
    refname: String,
}

/// A symbolic ref advertised by the remote: `name` (e.g. `HEAD`) points at
/// `target` (e.g. `refs/heads/main`).
#[derive(Debug, Clone, Serialize)]
struct LsRemoteSymref {
    name: String,
    target: String,
}

#[derive(Debug, Clone, Serialize)]
struct LsRemoteOutput {
    remote: String,
    url: String,
    heads_only: bool,
    tags_only: bool,
    refs_only: bool,
    get_url: bool,
    exit_code: bool,
    sort: Option<String>,
    patterns: Vec<String>,
    entries: Vec<LsRemoteEntry>,
    /// Symbolic-ref targets, populated only with `--symref`. Prefer advertised
    /// `symref=` capabilities; when they are absent, a visible HEAD may be
    /// derived from advertised branch tips (notably for local Libra sources).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    symrefs: Vec<LsRemoteSymref>,
}

/// Parse `symref=<from>:<to>` capability tokens advertised by `git-upload-pack`
/// into `(name, target)` pairs (e.g. `symref=HEAD:refs/heads/main` →
/// `("HEAD", "refs/heads/main")`). Capabilities without a `symref=` prefix or a
/// well-formed `from:to` body are ignored.
fn parse_symrefs(capabilities: &[String]) -> Vec<LsRemoteSymref> {
    capabilities
        .iter()
        .filter_map(|cap| {
            let body = cap.strip_prefix("symref=")?;
            let (name, target) = body.split_once(':')?;
            if name.is_empty() || target.is_empty() {
                return None;
            }
            Some(LsRemoteSymref {
                name: name.to_string(),
                target: target.to_string(),
            })
        })
        .collect()
}

/// Resolve the symbolic refs to surface for `--symref`: parse the remote's
/// advertised `symref=` capabilities (e.g. `symref=HEAD:refs/heads/main`) and
/// keep only those whose `name` survives the active ref filters. Returns empty
/// when `--symref` was not requested. When a transport has no `symref=`
/// capability (notably a local Libra source), derive HEAD from the advertised
/// HEAD and branch tips with the same deterministic resolver used by fetch.
fn resolve_output_symrefs(
    capabilities: &[String],
    entries: &[LsRemoteEntry],
    discovered: &[DiscRef],
    want: bool,
) -> Vec<LsRemoteSymref> {
    if !want {
        return Vec::new();
    }
    let parsed = parse_symrefs(capabilities)
        .into_iter()
        .filter(|symref| entries.iter().any(|entry| entry.refname == symref.name))
        .collect::<Vec<_>>();
    if !parsed.is_empty() {
        return parsed;
    }
    if !entries.iter().any(|entry| entry.refname == "HEAD") {
        return Vec::new();
    }
    let remote_head = discovered.iter().find(|reference| reference._ref == "HEAD");
    let heads = discovered
        .iter()
        .filter(|reference| reference._ref.starts_with("refs/heads/"))
        .cloned()
        .collect::<Vec<_>>();
    resolve_remote_default_branch(capabilities, &heads, remote_head)
        .map(|branch| {
            vec![LsRemoteSymref {
                name: "HEAD".to_string(),
                target: format!("refs/heads/{branch}"),
            }]
        })
        .unwrap_or_default()
}

#[derive(thiserror::Error, Debug)]
enum LsRemoteError {
    #[error("failed to read remote configuration: {0}")]
    ConfigRead(String),
    #[error("invalid remote '{spec}': {reason}")]
    InvalidRemote { spec: String, reason: String },
    #[error("invalid ref pattern '{pattern}': {reason}")]
    InvalidPattern { pattern: String, reason: String },
    #[error("unsupported ls-remote sort key '{0}'")]
    UnsupportedSortKey(String),
    #[error("failed to discover references from '{remote}': {source}")]
    Discovery { remote: String, source: GitError },
}

impl From<LsRemoteError> for CliError {
    fn from(error: LsRemoteError) -> Self {
        match &error {
            LsRemoteError::ConfigRead(_) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            LsRemoteError::InvalidRemote { .. } | LsRemoteError::InvalidPattern { .. } => {
                CliError::command_usage(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra remote -v' to inspect configured remotes")
            }
            LsRemoteError::UnsupportedSortKey(_) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use '--sort=refname' or '--sort=version:refname'."),
            LsRemoteError::Discovery { source, .. } => match source {
                GitError::UnAuthorized(_) => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::AuthPermissionDenied)
                    .with_hint("check SSH key / HTTP credentials and repository access rights"),
                GitError::NetworkError(detail) if detail.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX) => {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkProtocol)
                        .with_hint("check that the remote serves Git data and that a proxy has not altered the response")
                }
                GitError::IOError(source) if is_pkt_line_io_error(source) => {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkProtocol)
                        .with_hint("check that the remote serves Git data and that a proxy has not altered the response")
                }
                GitError::NetworkError(_) | GitError::IOError(_) => {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkUnavailable)
                        .with_hint("check the remote URL and network connectivity")
                }
                _ => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            },
        }
    }
}

pub async fn execute_safe(args: LsRemoteArgs, output: &OutputConfig) -> CliResult<()> {
    let data = run_ls_remote(args).await.map_err(CliError::from)?;
    render_ls_remote_output(&data, output)?;
    if data.exit_code && !data.get_url && data.entries.is_empty() {
        return Err(CliError::silent_exit(2));
    }
    Ok(())
}

async fn run_ls_remote(args: LsRemoteArgs) -> Result<LsRemoteOutput, LsRemoteError> {
    let (remote_display, remote_url, remote_name) = resolve_remote(&args.repository).await?;
    let visible_remote = visible_remote_display(&remote_display, remote_name.as_deref());
    if args.get_url {
        return Ok(LsRemoteOutput {
            remote: visible_remote,
            url: visible_remote_url(&remote_url),
            heads_only: args.heads,
            tags_only: args.tags,
            refs_only: args.refs,
            get_url: true,
            exit_code: args.exit_code,
            sort: args.sort,
            patterns: args.patterns,
            entries: Vec::new(),
            symrefs: Vec::new(),
        });
    }

    let client = RemoteClient::from_spec_with_remote(&remote_url, remote_name.as_deref())
        .await
        .map_err(|reason| LsRemoteError::InvalidRemote {
            spec: visible_remote.clone(),
            reason: sanitize_remote_error_reason(&reason, &remote_url),
        })?;
    let discovery = client
        .discovery_reference(UploadPack)
        .await
        .map_err(|source| LsRemoteError::Discovery {
            remote: visible_remote.clone(),
            source: sanitize_discovery_error(source, &remote_url),
        })?;
    let patterns = compile_patterns(&args.patterns)?;
    let mut entries: Vec<LsRemoteEntry> = discovery
        .refs
        .iter()
        .filter(|reference| include_reference(reference, &args, &patterns))
        .map(|reference| LsRemoteEntry {
            hash: reference._hash.clone(),
            refname: reference._ref.clone(),
        })
        .collect();
    sort_entries(&mut entries, args.sort.as_deref())?;

    let symrefs = resolve_output_symrefs(
        &discovery.capabilities,
        &entries,
        &discovery.refs,
        args.symref,
    );

    Ok(LsRemoteOutput {
        remote: visible_remote,
        url: visible_remote_url(&remote_url),
        heads_only: args.heads,
        tags_only: args.tags,
        refs_only: args.refs,
        get_url: false,
        exit_code: args.exit_code,
        sort: args.sort,
        patterns: args.patterns,
        entries,
        symrefs,
    })
}

async fn resolve_remote(
    repository: &str,
) -> Result<(String, String, Option<String>), LsRemoteError> {
    if is_unambiguous_direct_remote_spec(repository) {
        return Ok((repository.to_string(), repository.to_string(), None));
    }

    if util::try_get_storage_path(None).is_ok() {
        let configured = ConfigKv::remote_config(repository)
            .await
            .map_err(|error| LsRemoteError::ConfigRead(error.to_string()))?;
        if let Some(remote) = configured {
            return Ok((remote.name.clone(), remote.url, Some(remote.name)));
        }
    }

    Ok((repository.to_string(), repository.to_string(), None))
}

fn is_unambiguous_direct_remote_spec(repository: &str) -> bool {
    if is_ssh_spec(repository) || Url::parse(repository).is_ok() {
        return true;
    }

    let path = Path::new(repository);
    path.is_absolute()
        || repository.starts_with("./")
        || repository.starts_with("../")
        || repository.starts_with(".\\")
        || repository.starts_with("..\\")
}

fn render_ls_remote_output(data: &LsRemoteOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        emit_json_data("ls-remote", data, output)
    } else if output.quiet {
        Ok(())
    } else if data.get_url {
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        writeln!(writer, "{}", data.url)
            .map_err(|error| CliError::io(format!("failed to write ls-remote URL: {error}")))
    } else {
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        write_ref_lines(&mut writer, data)
            .map_err(|error| CliError::io(format!("failed to write ls-remote output: {error}")))
    }
}

/// Write the human-readable `<oid>\t<name>` ref lines, emitting a
/// `ref: <target>\t<name>` line immediately before a symref's own OID line
/// (matching `git ls-remote --symref`). Generic over the writer so the exact
/// line layout — including symref placement — is unit-testable.
fn write_ref_lines<W: Write>(writer: &mut W, data: &LsRemoteOutput) -> std::io::Result<()> {
    for entry in &data.entries {
        if let Some(symref) = data.symrefs.iter().find(|s| s.name == entry.refname) {
            writeln!(writer, "ref: {}\t{}", symref.target, symref.name)?;
        }
        writeln!(writer, "{}\t{}", entry.hash, entry.refname)?;
    }
    Ok(())
}

#[cfg(test)]
mod pkt_line_boundary_tests {
    use std::{
        fmt, io,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::Response,
    };
    use clap::Parser;
    use git_internal::errors::GitError;
    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

    use super::{LsRemoteArgs, LsRemoteError};
    use crate::{
        command::{
            clone::{self, CloneArgs, CloneError},
            fetch::{self, FetchArgs, FetchError},
            pull::{self, PullArgs, PullError},
        },
        git_protocol::{
            PKT_LINE_PROTOCOL_ERROR_PREFIX, ServiceType::UploadPack, add_pkt_line_string,
            read_pkt_line,
        },
        internal::{config::ConfigKv, protocol::parse_discovered_references},
        utils::{
            error::{CliError, StableErrorCode},
            output::OutputConfig,
            test::{ChangeDirGuard, setup_with_new_libra_in},
        },
    };

    const PROTOCOL_HINT: &str =
        "check that the remote serves Git data and that a proxy has not altered the response";
    const NETWORK_HINT: &str = "check network connectivity and retry";

    #[derive(Clone, Copy, Debug)]
    enum Boundary {
        LsRemote,
        CloneDiscovery,
        CloneObjects,
        ClonePacket,
        FetchObjects,
        FetchPacket,
        PullDiscovery,
        PullObjects,
        PullPacket,
    }
    const ALL: [Boundary; 9] = [
        Boundary::LsRemote,
        Boundary::CloneDiscovery,
        Boundary::CloneObjects,
        Boundary::ClonePacket,
        Boundary::FetchObjects,
        Boundary::FetchPacket,
        Boundary::PullDiscovery,
        Boundary::PullObjects,
        Boundary::PullPacket,
    ];

    fn discovery_boundary(boundary: Boundary, remote: &str, source: GitError) -> CliError {
        match boundary {
            Boundary::LsRemote => LsRemoteError::Discovery {
                remote: remote.to_string(),
                source,
            }
            .into(),
            Boundary::CloneDiscovery => CloneError::DiscoverRemote {
                source: FetchError::Discovery {
                    remote: remote.to_string(),
                    source,
                },
            }
            .into(),
            Boundary::PullDiscovery => PullError::Fetch(FetchError::Discovery {
                remote: remote.to_string(),
                source,
            })
            .into(),
            _ => panic!("not a discovery boundary: {boundary:?}"),
        }
    }

    fn io_boundary(boundary: Boundary, remote: &str, source: io::Error) -> CliError {
        match boundary {
            Boundary::LsRemote | Boundary::CloneDiscovery | Boundary::PullDiscovery => {
                discovery_boundary(boundary, remote, GitError::IOError(source))
            }
            Boundary::CloneObjects => CloneError::FetchFailed {
                source: FetchError::FetchObjects {
                    remote: remote.to_string(),
                    source,
                },
            }
            .into(),
            Boundary::ClonePacket => CloneError::FetchFailed {
                source: FetchError::PacketRead { source },
            }
            .into(),
            Boundary::FetchObjects => FetchError::FetchObjects {
                remote: remote.to_string(),
                source,
            }
            .into(),
            Boundary::FetchPacket => FetchError::PacketRead { source }.into(),
            Boundary::PullObjects => PullError::Fetch(FetchError::FetchObjects {
                remote: remote.to_string(),
                source,
            })
            .into(),
            Boundary::PullPacket => PullError::Fetch(FetchError::PacketRead { source }).into(),
        }
    }

    fn is_discovery(boundary: Boundary) -> bool {
        matches!(
            boundary,
            Boundary::LsRemote | Boundary::CloneDiscovery | Boundary::PullDiscovery
        )
    }

    fn parser_error(boundary: Boundary, remote: &str, wire: &[u8]) -> CliError {
        if is_discovery(boundary) {
            discovery_boundary(
                boundary,
                remote,
                parse_discovered_references(Bytes::copy_from_slice(wire), UploadPack)
                    .expect_err("malformed advertisement must fail in the parser"),
            )
        } else {
            let source = read_pkt_line(&mut Bytes::copy_from_slice(wire))
                .expect_err("malformed frame must fail in the parser");
            io_boundary(
                boundary,
                remote,
                io::Error::new(io::ErrorKind::InvalidData, source),
            )
        }
    }

    fn assert_protocol(boundary: Boundary, error: &CliError) {
        assert_eq!(
            error.stable_code(),
            StableErrorCode::NetworkProtocol,
            "{boundary:?}: {error:?}"
        );
        assert_eq!(error.stable_code().as_str(), "LBR-NET-002");
        assert_eq!(error.stable_code().exit_code().as_i32(), 128);
        let expected = if matches!(boundary, Boundary::FetchPacket | Boundary::PullPacket) {
            Vec::new()
        } else {
            vec![PROTOCOL_HINT]
        };
        assert_eq!(
            error
                .hints()
                .iter()
                .map(|hint| hint.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{boundary:?}"
        );
        if matches!(
            boundary,
            Boundary::PullDiscovery | Boundary::PullObjects | Boundary::PullPacket
        ) {
            assert_eq!(
                error.details().get("phase"),
                Some(&serde_json::json!("fetch"))
            );
        }
    }

    fn marker_cases(boundaries: &[Boundary], remote: &str) {
        for &boundary in boundaries {
            for wire in [b"0001".as_slice(), b"0", b"0008abc", b"SECR", b"\xff000"] {
                assert_protocol(boundary, &parser_error(boundary, remote, wire));
            }
            // The marker has priority over both timeout kind/text and the clone host-key heuristic.
            let detail =
                format!("{PKT_LINE_PROTOCOL_ERROR_PREFIX}timeout; host key verification failed");
            assert_protocol(
                boundary,
                &io_boundary(
                    boundary,
                    remote,
                    io::Error::new(io::ErrorKind::TimedOut, detail.clone()),
                ),
            );
            if is_discovery(boundary) {
                assert_protocol(
                    boundary,
                    &discovery_boundary(boundary, remote, GitError::NetworkError(detail)),
                );
            }
        }
    }

    fn non_marker_cases(boundaries: &[Boundary], remote: &str) {
        for &boundary in boundaries {
            for detail in [
                "connection reset".to_string(),
                "timed out".to_string(),
                format!("wrapper: {PKT_LINE_PROTOCOL_ERROR_PREFIX}invalid"),
                format!(" {PKT_LINE_PROTOCOL_ERROR_PREFIX}invalid"),
                format!("prefix {PKT_LINE_PROTOCOL_ERROR_PREFIX}invalid"),
                "PKT-LINE protocol error: invalid".to_string(),
            ] {
                for kind in [
                    io::ErrorKind::ConnectionReset,
                    io::ErrorKind::TimedOut,
                    io::ErrorKind::UnexpectedEof,
                    io::ErrorKind::InvalidData,
                ] {
                    let error = io_boundary(boundary, remote, io::Error::new(kind, detail.clone()));
                    let (code, hint) = match boundary {
                        Boundary::CloneDiscovery => (
                            StableErrorCode::IoReadFailed,
                            "check filesystem permissions and repository integrity",
                        ),
                        Boundary::LsRemote => (
                            StableErrorCode::NetworkUnavailable,
                            "check the remote URL and network connectivity",
                        ),
                        Boundary::CloneObjects | Boundary::ClonePacket => (
                            StableErrorCode::NetworkUnavailable,
                            "network error during transfer; check connectivity and retry",
                        ),
                        _ => (StableErrorCode::NetworkUnavailable, NETWORK_HINT),
                    };
                    assert_eq!(
                        error.stable_code(),
                        code,
                        "{boundary:?}/{kind:?}: {error:?}"
                    );
                    assert_eq!(error.stable_code().exit_code().as_i32(), 128);
                    assert_eq!(
                        error
                            .hints()
                            .iter()
                            .map(|hint| hint.as_str())
                            .collect::<Vec<_>>(),
                        [hint]
                    );
                }
                if is_discovery(boundary) {
                    let error =
                        discovery_boundary(boundary, remote, GitError::NetworkError(detail));
                    assert_eq!(
                        error.stable_code(),
                        StableErrorCode::NetworkUnavailable,
                        "{boundary:?}: {error:?}"
                    );
                    let hint = match boundary {
                        Boundary::LsRemote => "check the remote URL and network connectivity",
                        Boundary::CloneDiscovery => {
                            "check the remote host, DNS, VPN/proxy, and network connectivity"
                        }
                        _ => NETWORK_HINT,
                    };
                    assert_eq!(
                        error
                            .hints()
                            .iter()
                            .map(|hint| hint.as_str())
                            .collect::<Vec<_>>(),
                        [hint]
                    );
                }
            }
            if is_discovery(boundary) {
                let error = discovery_boundary(
                    boundary,
                    remote,
                    GitError::UnAuthorized("permission denied".to_string()),
                );
                assert_eq!(error.stable_code(), StableErrorCode::AuthPermissionDenied);
                assert_eq!(
                    error
                        .hints()
                        .iter()
                        .map(|hint| hint.as_str())
                        .collect::<Vec<_>>(),
                    ["check SSH key / HTTP credentials and repository access rights"]
                );
            }
        }
    }

    #[test]
    fn pkt_line_matrix_ls_remote_marker_maps_net_002() {
        marker_cases(&[Boundary::LsRemote], "origin");
        let remote = "https://user:credential_7c41@example.invalid/repo";
        let source = read_pkt_line(&mut Bytes::from_static(b"SECR")).unwrap_err();
        for source in [
            GitError::NetworkError(source.to_string()),
            GitError::IOError(io::Error::new(io::ErrorKind::InvalidData, source)),
        ] {
            let sanitized = super::sanitize_discovery_error(source, remote);
            let error = discovery_boundary(Boundary::LsRemote, "origin", sanitized);
            assert_protocol(Boundary::LsRemote, &error);
            assert!(!error.render_report().contains("credential_7c41"));
        }
    }
    #[test]
    fn pkt_line_matrix_clone_discovery_marker_maps_net_002() {
        marker_cases(&[Boundary::CloneDiscovery], "origin");
    }
    #[test]
    fn pkt_line_matrix_clone_fetch_phase_marker_maps_net_002() {
        marker_cases(&[Boundary::CloneObjects, Boundary::ClonePacket], "origin");
    }
    #[test]
    fn pkt_line_matrix_fetch_objects_marker_maps_net_002() {
        marker_cases(&[Boundary::FetchObjects], "origin");
    }
    #[test]
    fn pkt_line_matrix_pull_discovery_marker_maps_net_002() {
        marker_cases(&[Boundary::PullDiscovery], "origin");
    }
    #[test]
    fn pkt_line_matrix_pull_fetch_objects_marker_maps_net_002() {
        marker_cases(&[Boundary::PullObjects], "origin");
    }
    #[test]
    fn pkt_line_matrix_non_marker_ls_remote_stays_net_001() {
        non_marker_cases(&[Boundary::LsRemote], "origin");
        let error: CliError =
            LsRemoteError::ConfigRead("unreadable local config".to_string()).into();
        assert_eq!(error.stable_code(), StableErrorCode::IoReadFailed);
    }
    #[test]
    fn pkt_line_matrix_non_marker_clone_discovery_stays_net_001() {
        use crate::internal::protocol::ssh_client::{
            SSH_HOST_KEY_CHANGED_GUIDANCE, SSH_HOST_KEY_CHANGED_SIGNAL, SSH_HOST_KEY_GUIDANCE,
            SSH_HOST_KEY_UNCONFIRMED_SIGNAL,
        };

        non_marker_cases(&[Boundary::CloneDiscovery], "origin");
        // Only the local carrier at the start of the inner NetworkError selects
        // host guidance; raw diagnostic lookalikes remain ordinary network errors.
        for detail in [
            "Host key verification failed.".to_string(),
            "REMOTE HOST IDENTIFICATION HAS CHANGED".to_string(),
            format!("context: {SSH_HOST_KEY_UNCONFIRMED_SIGNAL}lookalike"),
            format!("context: {SSH_HOST_KEY_CHANGED_SIGNAL}lookalike"),
        ] {
            let error = discovery_boundary(
                Boundary::CloneDiscovery,
                "ssh://example.invalid/repo",
                GitError::NetworkError(detail),
            );
            assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
            assert_eq!(error.exit_code(), 128);
            assert_eq!(
                error
                    .hints()
                    .iter()
                    .map(|hint| hint.as_str())
                    .collect::<Vec<_>>(),
                ["check the remote host, DNS, VPN/proxy, and network connectivity"]
            );
        }
        for (signal, message, guidance) in [
            (
                SSH_HOST_KEY_UNCONFIRMED_SIGNAL,
                "SSH host key could not be verified",
                SSH_HOST_KEY_GUIDANCE,
            ),
            (
                SSH_HOST_KEY_CHANGED_SIGNAL,
                "SSH host identity has changed",
                SSH_HOST_KEY_CHANGED_GUIDANCE,
            ),
        ] {
            let error = discovery_boundary(
                Boundary::CloneDiscovery,
                "ssh://example.invalid/repo",
                GitError::NetworkError(format!("{signal}{message}; MATRIX_HOST_SENTINEL")),
            );
            assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
            assert_eq!(error.exit_code(), 128);
            assert_eq!(error.message(), message);
            assert_eq!(
                error
                    .hints()
                    .iter()
                    .map(|hint| hint.as_str())
                    .collect::<Vec<_>>(),
                [guidance]
            );
            assert!(error.hints()[0].as_str().contains("~/.ssh/known_hosts"));
            for rendered in [error.render(), error.render_report(), error.render_json()] {
                assert!(!rendered.contains(signal));
                assert!(!rendered.contains("MATRIX_HOST_SENTINEL"));
            }
        }
    }
    #[test]
    fn pkt_line_matrix_non_marker_clone_fetch_phase_stays_net_001() {
        non_marker_cases(&[Boundary::CloneObjects, Boundary::ClonePacket], "origin");
        let error = CliError::from(CloneError::FetchFailed {
            source: FetchError::IncompletePack { received: 3 },
        });
        assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
        assert_eq!(
            error
                .hints()
                .iter()
                .map(|hint| hint.as_str())
                .collect::<Vec<_>>(),
            ["network error during transfer; check connectivity and retry"]
        );
    }
    #[test]
    fn pkt_line_matrix_non_marker_fetch_objects_stays_net_001() {
        non_marker_cases(&[Boundary::FetchObjects], "origin");
    }
    #[test]
    fn pkt_line_matrix_non_marker_pull_discovery_stays_net_001() {
        non_marker_cases(&[Boundary::PullDiscovery], "origin");
    }
    #[test]
    fn pkt_line_matrix_non_marker_pull_fetch_objects_stays_net_001() {
        non_marker_cases(&[Boundary::PullObjects], "origin");
    }
    #[test]
    fn pkt_line_matrix_parametrized_git_marker_maps_net_002() {
        marker_cases(&ALL, "git://example.invalid/repo");
    }
    #[test]
    fn pkt_line_matrix_parametrized_git_non_marker_stays_net_001() {
        non_marker_cases(&ALL, "git://example.invalid/repo");
    }
    #[test]
    fn pkt_line_matrix_parametrized_ssh_marker_maps_net_002() {
        marker_cases(&ALL, "ssh://git@example.invalid/repo");
    }
    #[test]
    fn pkt_line_matrix_parametrized_ssh_non_marker_stays_net_001() {
        non_marker_cases(&ALL, "ssh://git@example.invalid/repo");
    }
    #[test]
    fn pkt_line_matrix_parametrized_https_non_marker_stays_net_001() {
        non_marker_cases(&ALL, "https://example.invalid/repo");
        // A marker in the displayed remote must never override a non-marker inner error.
        non_marker_cases(
            &ALL,
            &format!("https://example.invalid/{PKT_LINE_PROTOCOL_ERROR_PREFIX}timeout"),
        );
    }
    #[test]
    fn pkt_line_matrix_fetch_packet_read_non_marker_io_stays_net_001() {
        non_marker_cases(&[Boundary::FetchPacket], "origin");
    }
    #[test]
    fn pkt_line_matrix_pull_packet_read_non_marker_io_stays_net_001() {
        non_marker_cases(&[Boundary::PullPacket], "origin");
    }
    #[test]
    fn pkt_line_matrix_fetch_packet_read_marker_maps_net_002() {
        marker_cases(&[Boundary::FetchPacket], "origin");
    }
    #[test]
    fn pkt_line_matrix_pull_packet_read_marker_maps_net_002() {
        marker_cases(&[Boundary::PullPacket], "origin");
    }

    #[test]
    fn pkt_line_matrix_zero_echo_sentinel() {
        const SENTINEL: &str = "REMOTE_BOUNDARY_SECRET_7c41";
        for boundary in ALL {
            for header in [b"SECR".as_slice(), b"\xff000", b"0001", b"ffff"] {
                let wire = [header, SENTINEL.as_bytes()].concat();
                let error = parser_error(boundary, "https://example.invalid/repo", &wire);
                assert_protocol(boundary, &error);
                for rendered in [error.render(), error.render_report(), error.render_json()] {
                    assert!(!rendered.contains(SENTINEL), "{boundary:?}: {rendered}");
                    assert!(!rendered.contains("SECR"), "{boundary:?}: {rendered}");
                }
            }
        }
    }

    #[test]
    fn pkt_line_matrix_marker_constant_single_source() {
        let forbidden = ["pkt-line", " protocol error: \""].concat();
        for source in [
            include_str!("ls_remote.rs"),
            include_str!("clone.rs"),
            include_str!("fetch.rs"),
            include_str!("pull.rs"),
        ] {
            assert!(source.contains("PKT_LINE_PROTOCOL_ERROR_PREFIX"));
            assert!(
                !source.contains(&forbidden),
                "mapper duplicates marker literal"
            );
        }
        #[derive(Debug)]
        struct Chunked {
            text: String,
            suffix_formatted: Arc<AtomicBool>,
        }
        impl fmt::Display for Chunked {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for ch in self.text.chars() {
                    write!(f, "{ch}")?;
                }
                self.suffix_formatted.store(true, Ordering::SeqCst);
                f.write_str("never needed by the prefix classifier")
            }
        }
        impl std::error::Error for Chunked {}
        for (text, expected) in [
            (PKT_LINE_PROTOCOL_ERROR_PREFIX.to_string(), true),
            (format!(" {PKT_LINE_PROTOCOL_ERROR_PREFIX}"), false),
            (format!("é{PKT_LINE_PROTOCOL_ERROR_PREFIX}"), false),
            (PKT_LINE_PROTOCOL_ERROR_PREFIX[..8].to_string(), false),
        ] {
            let suffix = Arc::new(AtomicBool::new(false));
            let error = io::Error::other(Chunked {
                text: text.clone(),
                suffix_formatted: suffix.clone(),
            });
            assert_eq!(fetch::is_pkt_line_io_error(&error), expected, "{text:?}");
            if expected || text.starts_with(' ') || text.starts_with('é') {
                assert!(!suffix.load(Ordering::SeqCst));
            }
        }
        assert!(!fetch::is_pkt_line_io_error(&io::Error::from(
            io::ErrorKind::ConnectionReset
        )));
    }

    // HTTP is routed through the production HttpsClient. This tests Git protocol
    // framing and command adapters, not TLS negotiation or certificate validation.
    // Both command-path tests use two runtime workers: synchronous timeout-config
    // lookups must leave the cached SQL pool and HTTP server able to make progress.
    const OID: &str = "1111111111111111111111111111111111111111";
    #[derive(Clone, Copy)]
    enum ResponseMode {
        EmptyAdvertisement,
        MalformedFetch,
    }
    type Transcript = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;
    #[derive(Clone)]
    struct ServerState {
        mode: ResponseMode,
        transcript: Transcript,
    }
    struct TestServer {
        url: String,
        transcript: Transcript,
        task: JoinHandle<()>,
        shutdown: Option<oneshot::Sender<()>>,
    }
    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            self.task.abort();
        }
    }

    async fn respond(State(state): State<ServerState>, request: Request<Body>) -> Response {
        let method = request.method().to_string();
        let path = request
            .uri()
            .path_and_query()
            .expect("mock URI")
            .to_string();
        let body = to_bytes(request.into_body(), 64 * 1024)
            .await
            .expect("bounded mock request");
        state
            .transcript
            .lock()
            .unwrap()
            .push((method.clone(), path.clone(), body.to_vec()));
        let (content_type, response) = if method == "GET"
            && path == "/repo/info/refs?service=git-upload-pack"
        {
            let mut bytes = bytes::BytesMut::new();
            if matches!(state.mode, ResponseMode::MalformedFetch) {
                add_pkt_line_string(&mut bytes, "# service=git-upload-pack\n".to_string());
                bytes.extend_from_slice(b"0000");
                add_pkt_line_string(
                    &mut bytes,
                    format!(
                        "{OID} HEAD\0multi_ack_detailed side-band-64k ofs-delta symref=HEAD:refs/heads/main object-format=sha1\n"
                    ),
                );
                add_pkt_line_string(&mut bytes, format!("{OID} refs/heads/main\n"));
                bytes.extend_from_slice(b"0000");
            }
            (
                "application/x-git-upload-pack-advertisement",
                bytes.to_vec(),
            )
        } else if method == "POST" && path == "/repo/git-upload-pack" {
            (
                "application/x-git-upload-pack-result",
                b"0008NAK\n0001REMOTE_BOUNDARY_SECRET_7c41".to_vec(),
            )
        } else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .body(Body::from(response))
            .unwrap()
    }

    impl TestServer {
        async fn start(mode: ResponseMode) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let transcript = Arc::new(Mutex::new(Vec::new()));
            let state = ServerState {
                mode,
                transcript: transcript.clone(),
            };
            let app = Router::new().fallback(respond).with_state(state);
            let (shutdown, shutdown_requested) = oneshot::channel();
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_requested.await;
                    })
                    .await
                    .expect("mock server should remain healthy");
            });
            Self {
                url: format!("http://{address}/repo/"),
                transcript,
                task,
                shutdown: Some(shutdown),
            }
        }
        fn requests(&self) -> Vec<(String, String, Vec<u8>)> {
            self.transcript.lock().unwrap().clone()
        }
        async fn stop(&mut self) {
            self.shutdown
                .take()
                .expect("mock stopped once")
                .send(())
                .expect("mock still running");
            tokio::time::timeout(Duration::from_secs(5), &mut self.task)
                .await
                .expect("mock shutdown bounded")
                .expect("mock shutdown succeeds");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial(env, cwd, hash_kind)]
    async fn pkt_line_matrix_parametrized_https_marker_maps_net_002() {
        marker_cases(&ALL, "https://example.invalid/repo");
        let parent = tempdir().unwrap();
        setup_with_new_libra_in(parent.path()).await;
        let _cwd = ChangeDirGuard::new(parent.path());
        let mut server = TestServer::start(ResponseMode::MalformedFetch).await;
        let destination = parent.path().join("clone-target");
        let args = CloneArgs::try_parse_from([
            "clone",
            server.url.as_str(),
            destination.to_str().unwrap(),
        ])
        .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(45),
            clone::execute_safe(args, &OutputConfig::default()),
        )
        .await
        .expect("clone bounded")
        .expect_err("bad fetch frame must fail");
        assert_protocol(Boundary::ClonePacket, &error);
        assert!(
            error.message().contains("failed to read fetch stream"),
            "{error:?}"
        );
        assert!(
            !error
                .render_report()
                .contains("REMOTE_BOUNDARY_SECRET_7c41")
        );
        let requests = server.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.0 == "GET" && r.1 == "/repo/info/refs?service=git-upload-pack")
                .count(),
            2,
            "{requests:?}"
        );
        let posts = requests
            .iter()
            .filter(|r| r.0 == "POST" && r.1 == "/repo/git-upload-pack")
            .collect::<Vec<_>>();
        assert_eq!(posts.len(), 1, "{requests:?}");
        assert!(String::from_utf8_lossy(&posts[0].2).contains(&format!("want {OID}")));
        assert_eq!(requests.len(), 3, "{requests:?}");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial(env, cwd, hash_kind)]
    async fn pkt_line_discovery_empty_response_regression_fetch_clone_lsremote_pull() {
        let mut server = TestServer::start(ResponseMode::EmptyAdvertisement).await;
        for command in ["fetch", "clone", "ls-remote", "pull"] {
            let repo = tempdir().unwrap();
            setup_with_new_libra_in(repo.path()).await;
            let _cwd = ChangeDirGuard::new(repo.path());
            if matches!(command, "fetch" | "pull") {
                ConfigKv::set("remote.origin.url", &server.url, false)
                    .await
                    .unwrap();
            }
            let output = OutputConfig::default();
            let destination = repo.path().join("clone-target");
            let before = server.requests().len();
            let error = tokio::time::timeout(Duration::from_secs(45), async {
                match command {
                    "fetch" => fetch::execute_safe(
                        FetchArgs::try_parse_from(["fetch", "origin"]).unwrap(),
                        &output,
                    )
                    .await
                    .unwrap_err(),
                    "clone" => clone::execute_safe(
                        CloneArgs::try_parse_from([
                            "clone",
                            server.url.as_str(),
                            destination.to_str().unwrap(),
                        ])
                        .unwrap(),
                        &output,
                    )
                    .await
                    .unwrap_err(),
                    "ls-remote" => super::execute_safe(
                        LsRemoteArgs::try_parse_from(["ls-remote", server.url.as_str()]).unwrap(),
                        &output,
                    )
                    .await
                    .unwrap_err(),
                    "pull" => pull::execute_safe(
                        PullArgs::try_parse_from(["pull", "--ff-only", "origin", "main"]).unwrap(),
                        &output,
                    )
                    .await
                    .unwrap_err(),
                    _ => unreachable!(),
                }
            })
            .await
            .expect("empty advertisement command must terminate");
            assert_eq!(
                error.stable_code(),
                StableErrorCode::NetworkProtocol,
                "{command}: {error:?}"
            );
            assert_eq!(error.stable_code().exit_code().as_i32(), 128);
            assert!(
                error.message().contains(&format!(
                    "{PKT_LINE_PROTOCOL_ERROR_PREFIX}empty discovery response"
                )),
                "{command}: {error:?}"
            );
            assert_eq!(
                error
                    .hints()
                    .iter()
                    .map(|hint| hint.as_str())
                    .collect::<Vec<_>>(),
                [PROTOCOL_HINT]
            );
            if command == "pull" {
                assert_eq!(
                    error.details().get("phase"),
                    Some(&serde_json::json!("fetch"))
                );
            }
            let requests = server.requests();
            assert_eq!(requests.len(), before + 1, "{command}: {requests:?}");
            assert_eq!(
                (&requests[before].0[..], &requests[before].1[..]),
                ("GET", "/repo/info/refs?service=git-upload-pack")
            );
        }
        server.stop().await;
    }
}
