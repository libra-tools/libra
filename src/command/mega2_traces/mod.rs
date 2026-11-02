//! `libra mega2 traces` read and explicit fetch commands.
//!
//! Queries are GETs under `/api/v1/traces`. The command does not open a
//! repository database except for `fetch`, which records one archive ref
//! after the pack closure checks succeed.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::{
    internal::{
        db::{begin_write_transaction, get_db_conn_instance},
        model::reference::{self, ConfigKind},
        protocol::{
            mega2_auth::resolve_token,
            mega2_traces::{
                self, HashKind, METADATA_LIMIT_DEFAULT, METADATA_LIMIT_MAX, PROTOCOL_ID,
                RESPONSE_BYTES_MAX, SEARCH_LIMIT_DEFAULT, SEARCH_LIMIT_MAX, TEXT_BYTES_DEFAULT,
                TEXT_BYTES_MAX,
            },
            mega2_traces_git::{self, ArchiveObject},
            mega2_tree::validate_server_url,
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{CommandOutput, OutputConfig},
    },
};

mod search;

#[derive(Args, Debug)]
pub struct TracesArgs {
    #[command(subcommand)]
    pub command: TracesCommand,
}

#[derive(Subcommand, Debug)]
pub enum TracesCommand {
    /// Show the authenticated traces protocol and search readiness.
    Capabilities(ReadArgs),
    /// Resolve an authorized target path. This does not create a scope.
    Targets(TargetArgs),
    /// List one page of streams for a target.
    Streams(StreamArgs),
    /// List one page of sessions for a stream.
    Sessions(SessionArgs),
    /// Show one session or one checkpoint.
    Show(ShowArgs),
    /// List checkpoints for a session.
    Checkpoints(CheckpointArgs),
    /// Search redacted transcript text.
    Search(SearchArgs),
    /// Read a bounded slice of one checkpoint transcript.
    Transcript(TranscriptArgs),
    /// Fetch one verified traces stream into an archive remote-tracking ref.
    Fetch(FetchArgs),
}

#[derive(Args, Debug)]
pub struct ReadArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    /// Fail when the service reports search_ready=false.
    #[arg(long)]
    pub require_search: bool,
}

#[derive(Args, Debug)]
pub struct EndpointArgs {
    /// Mega2 HTTP base URL.
    #[arg(long, value_name = "BASE-URL")]
    pub server: String,
    /// Read the bearer token from this file.
    #[arg(long = "token-file", value_name = "PATH")]
    pub token_file: Option<PathBuf>,
    /// Bearer token. Prefer `--token-file`.
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    /// Canonical project path, for example `/project/foo`.
    #[arg(long, value_name = "PATH")]
    pub path: String,
}

#[derive(Args, Debug)]
pub struct StreamArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    /// Target UUID returned by `targets`.
    #[arg(long, value_name = "UUID")]
    pub target: String,
    /// Source repository id. Passed through unchanged.
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
    /// Owner principal UUID. Passed through unchanged.
    #[arg(long, value_name = "UUID")]
    pub owner: Option<String>,
    /// Epoch UUID. Passed through unchanged.
    #[arg(long, value_name = "UUID")]
    pub epoch: Option<String>,
    /// Resume one server page. Passed through unchanged.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
}

#[derive(Args, Debug)]
pub struct SessionArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    #[arg(long, value_name = "UUID")]
    pub stream: String,
    /// Inclusive RFC3339 lower bound. Sent as UTC.
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    /// Exclusive RFC3339 upper bound. Sent as UTC.
    #[arg(long, value_name = "RFC3339")]
    pub until: Option<String>,
    /// `metadata.agent_kind` value, for example `claude_code`.
    #[arg(long, value_name = "KIND")]
    pub agent: Option<String>,
    /// Full 40- or 64-character code OID.
    #[arg(long = "code-commit", value_name = "OID")]
    pub code_commit: Option<String>,
    /// Session id. Passed through unchanged.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Resume one server page. Passed through unchanged.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    #[arg(long, value_name = "UUID")]
    pub stream: String,
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    #[arg(long, value_name = "ID")]
    pub checkpoint: Option<String>,
}

#[derive(Args, Debug)]
pub struct CheckpointArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    #[arg(long, value_name = "UUID")]
    pub stream: String,
    #[arg(long, value_name = "ID")]
    pub session: String,
    /// Resume one server page. Passed through unchanged.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    /// Project path. The command resolves it before searching.
    #[arg(long, value_name = "PATH")]
    pub target: String,
    /// Query text, measured in UTF-8 bytes. The server applies ASCII case-fold.
    #[arg(long, value_name = "TEXT", default_value = "")]
    pub query: String,
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
    #[arg(long, value_name = "UUID")]
    pub stream: Option<String>,
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    #[arg(long, value_name = "KIND")]
    pub agent: Option<String>,
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    #[arg(long, value_name = "RFC3339")]
    pub until: Option<String>,
    #[arg(long = "code-commit", value_name = "OID")]
    pub code_commit: Option<String>,
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    #[arg(long, value_name = "BYTES")]
    pub max_bytes: Option<u64>,
}

#[derive(Args, Debug)]
pub struct TranscriptArgs {
    #[command(flatten)]
    pub endpoint: EndpointArgs,
    #[arg(long, value_name = "UUID")]
    pub stream: String,
    #[arg(long, value_name = "ID")]
    pub checkpoint: String,
    #[arg(long, value_name = "BYTES")]
    pub max_bytes: Option<u64>,
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct FetchArgs {
    /// Remote whose URL identifies the Mega2 code endpoint.
    #[arg(long, value_name = "NAME")]
    pub remote: String,
    /// Stream UUID.
    #[arg(long, value_name = "UUID")]
    pub stream: String,
    #[arg(long = "token-file", value_name = "PATH")]
    pub token_file: Option<PathBuf>,
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TracesReport {
    pub operation: &'static str,
    pub body: Value,
}

impl CommandOutput for TracesReport {
    fn render_human(
        &self,
        writer: &mut dyn std::io::Write,
        _config: &OutputConfig,
    ) -> std::io::Result<()> {
        writeln!(writer, "{}", self.operation)?;
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&self.body).unwrap_or_default()
        )
    }
}

/// Run one traces subcommand.
///
/// # Side Effects
///
/// Read commands only send HTTP GETs. `fetch` writes one archive
/// remote-tracking ref and repository config after pack validation.
/// It does not update `refs/heads/traces` or `refs/libra/traces`.
///
/// # Errors
///
/// Invalid URLs, tokens, query bounds, time windows, oversized bodies,
/// cross-origin credential forwarding, and unverified archives stop the
/// command. Search text outside 1..=256 bytes never opens a socket.
pub async fn execute_safe(args: TracesArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        TracesCommand::Capabilities(args) => capabilities(args, output).await,
        TracesCommand::Targets(args) => targets(args, output).await,
        TracesCommand::Streams(args) => streams(args, output).await,
        TracesCommand::Sessions(args) => sessions(args, output).await,
        TracesCommand::Show(args) => show(args, output).await,
        TracesCommand::Checkpoints(args) => checkpoints(args, output).await,
        TracesCommand::Search(args) => search_command(args, output).await,
        TracesCommand::Transcript(args) => transcript(args, output).await,
        TracesCommand::Fetch(args) => fetch_command(args, output).await,
    }
}

async fn capabilities(args: ReadArgs, output: &OutputConfig) -> CliResult<()> {
    let body = get_json(&args.endpoint, "/api/v1/traces/capabilities", &[]).await?;
    let protocol = body.get("protocol").and_then(Value::as_str).unwrap_or("");
    if protocol != PROTOCOL_ID {
        return Err(CliError::fatal(format!(
            "traces server protocol {protocol:?} is not {PROTOCOL_ID}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    if args.require_search {
        let reason = body
            .get("search_unavailable_reason")
            .and_then(Value::as_str);
        if let Err(message) = mega2_traces::search_ready_gate(body.get("search_ready"), reason) {
            return Err(
                CliError::fatal(message).with_stable_code(StableErrorCode::NetworkUnavailable)
            );
        }
    }
    emit_report("capabilities", body, output)
}

async fn targets(args: TargetArgs, output: &OutputConfig) -> CliResult<()> {
    let path = mega2_traces::normalize_target_path(&args.path).map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    let body = get_json(
        &args.endpoint,
        "/api/v1/traces/targets/resolve",
        &[("path", path.as_str())],
    )
    .await?;
    emit_report("targets", body, output)
}

async fn streams(args: StreamArgs, output: &OutputConfig) -> CliResult<()> {
    let limit = count(
        "limit",
        args.limit,
        METADATA_LIMIT_DEFAULT,
        METADATA_LIMIT_MAX,
    )?;
    let mut owned = vec![("limit".to_string(), limit.to_string())];
    push_owned(&mut owned, "source", args.source);
    push_owned(&mut owned, "owner", args.owner);
    push_owned(&mut owned, "epoch", args.epoch);
    push_owned(&mut owned, "cursor", args.cursor);
    let query = borrowed(&owned);
    let path = format!("/api/v1/traces/targets/{}/streams", args.target);
    let body = get_json(&args.endpoint, &path, &query).await?;
    emit_report("streams", body, output)
}

async fn sessions(args: SessionArgs, output: &OutputConfig) -> CliResult<()> {
    admit_code_commit(args.code_commit.as_deref())?;
    let since = optional_time(args.since.as_deref())?;
    let until = optional_time(args.until.as_deref())?;
    mega2_traces::validate_time_window(since, until).map_err(argument_error)?;
    let since_wire = utc_bound(args.since.as_deref())?;
    let until_wire = utc_bound(args.until.as_deref())?;
    let limit = count(
        "limit",
        args.limit,
        METADATA_LIMIT_DEFAULT,
        METADATA_LIMIT_MAX,
    )?;
    let mut owned = vec![("limit".to_string(), limit.to_string())];
    push_owned(&mut owned, "since", since_wire);
    push_owned(&mut owned, "until", until_wire);
    push_owned(&mut owned, "agent", args.agent);
    push_owned(&mut owned, "code_commit", args.code_commit);
    push_owned(&mut owned, "session", args.session);
    push_owned(&mut owned, "cursor", args.cursor);
    let query = borrowed(&owned);
    let path = format!("/api/v1/traces/streams/{}/sessions", args.stream);
    let body = get_json(&args.endpoint, &path, &query).await?;
    emit_report("sessions", body, output)
}

async fn show(args: ShowArgs, output: &OutputConfig) -> CliResult<()> {
    if args.session.is_some() && args.checkpoint.is_some() {
        return Err(argument_error(
            "show accepts either --session or --checkpoint, not both; the request was not sent"
                .to_string(),
        ));
    }
    let path = if let Some(checkpoint) = args.checkpoint.as_deref() {
        format!(
            "/api/v1/traces/streams/{}/checkpoints/{checkpoint}",
            args.stream
        )
    } else if let Some(session) = args.session.as_deref() {
        format!("/api/v1/traces/streams/{}/sessions/{session}", args.stream)
    } else {
        return Err(argument_error(
            "show requires --session or --checkpoint".to_string(),
        ));
    };
    let body = get_json(&args.endpoint, &path, &[]).await?;
    emit_report("show", body, output)
}

async fn checkpoints(args: CheckpointArgs, output: &OutputConfig) -> CliResult<()> {
    let limit = count(
        "limit",
        args.limit,
        METADATA_LIMIT_DEFAULT,
        METADATA_LIMIT_MAX,
    )?;
    let mut owned = vec![("limit".to_string(), limit.to_string())];
    push_owned(&mut owned, "cursor", args.cursor);
    let query = borrowed(&owned);
    let path = format!(
        "/api/v1/traces/streams/{}/sessions/{}/checkpoints",
        args.stream, args.session
    );
    let body = get_json(&args.endpoint, &path, &query).await?;
    emit_report("checkpoints", body, output)
}

async fn search_command(args: SearchArgs, output: &OutputConfig) -> CliResult<()> {
    search::admit_search_query(&args.query).map_err(argument_error)?;
    admit_code_commit(args.code_commit.as_deref())?;
    let since = optional_time(args.since.as_deref())?;
    let until = optional_time(args.until.as_deref())?;
    mega2_traces::validate_time_window(since, until).map_err(argument_error)?;
    let since_wire = utc_bound(args.since.as_deref())?;
    let until_wire = utc_bound(args.until.as_deref())?;
    let limit = count("limit", args.limit, SEARCH_LIMIT_DEFAULT, SEARCH_LIMIT_MAX)?;
    let max_bytes = count(
        "max_bytes",
        args.max_bytes,
        TEXT_BYTES_DEFAULT,
        TEXT_BYTES_MAX,
    )?;
    let resolved = get_json(
        &args.endpoint,
        "/api/v1/traces/targets/resolve",
        &[("path", args.target.as_str())],
    )
    .await?;
    let target_id = resolved
        .get("target_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::fatal("target resolve response has no target_id".to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?
        .to_string();
    let mut owned = vec![
        ("target_id".to_string(), target_id),
        ("q".to_string(), args.query),
        ("limit".to_string(), limit.to_string()),
        ("max_bytes".to_string(), max_bytes.to_string()),
    ];
    push_owned(&mut owned, "source", args.source);
    push_owned(&mut owned, "stream", args.stream);
    push_owned(&mut owned, "session", args.session);
    push_owned(&mut owned, "agent", args.agent);
    push_owned(&mut owned, "since", since_wire);
    push_owned(&mut owned, "until", until_wire);
    push_owned(&mut owned, "code_commit", args.code_commit);
    push_owned(&mut owned, "cursor", args.cursor);
    let query = borrowed(&owned);
    let body = get_json(&args.endpoint, "/api/v1/traces/search", &query).await?;
    emit_report("search", body, output)
}

async fn transcript(args: TranscriptArgs, output: &OutputConfig) -> CliResult<()> {
    let max_bytes = count(
        "max_bytes",
        args.max_bytes,
        TEXT_BYTES_DEFAULT,
        TEXT_BYTES_MAX,
    )?;
    let bytes_text = max_bytes.to_string();
    let mut owned = vec![("max_bytes".to_string(), bytes_text)];
    if let Some(cursor) = args.cursor.clone() {
        owned.push(("cursor".to_string(), cursor));
    }
    let query: Vec<(&str, &str)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let path = format!(
        "/api/v1/traces/streams/{}/checkpoints/{}/transcript",
        args.stream, args.checkpoint
    );
    let body = get_json(&args.endpoint, &path, &query).await?;
    emit_report("transcript", body, output)
}

async fn fetch_command(args: FetchArgs, output: &OutputConfig) -> CliResult<()> {
    let workdir = std::env::current_dir().map_err(|error| {
        CliError::fatal(format!("cannot resolve the working directory: {error}"))
    })?;
    let url_key = format!("remote.{}.url", args.remote);
    let stored = crate::command::agent::mega2_stream::repository_config_at(&workdir, &url_key)
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    let Some(remote_url) = stored.filter(|value| !value.is_empty()) else {
        return Err(argument_error(format!(
            "remote '{}' has no URL; traces fetch stopped before any network attempt",
            args.remote
        )));
    };
    let token = resolve_token(
        args.token_file.as_deref(),
        std::env::var(crate::internal::protocol::mega2_auth::ENV_TOKEN_VAR)
            .ok()
            .as_deref(),
        args.token.as_deref(),
    )?;
    if token.is_none() {
        return Err(CliError::fatal(
            "anonymous traces fetch is refused; no archive ref was written".to_string(),
        )
        .with_stable_code(StableErrorCode::AuthMissingCredentials));
    }
    let remote_url_parsed = Url::parse(&remote_url).map_err(|_| {
        CliError::fatal(format!(
            "remote URL {remote_url:?} is not an HTTP(S) or SSH Mega2 endpoint"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    if remote_url_parsed.scheme() == "ssh" {
        let locator = mega2_traces_git::ssh_stream_locator(&remote_url, &args.stream)
            .map_err(argument_error)?;
        return Err(CliError::fatal(format!(
            "SSH traces discovery located {locator} but no authenticated git advertisement was read; no archive ref was written"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    let authority = mega2_traces_git::transport_authority(&remote_url).map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let base = remote_http_origin(&remote_url)?;
    let endpoint = EndpointArgs {
        server: base,
        token_file: args.token_file,
        token: args.token,
    };
    let capabilities = get_json(&endpoint, "/api/v1/traces/capabilities", &[]).await?;
    let protocol = capabilities
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("");
    if protocol != PROTOCOL_ID {
        return Err(CliError::fatal(format!(
            "traces server protocol {protocol:?} is not {PROTOCOL_ID}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    let server_id = capabilities
        .get("server_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::fatal(
                "authenticated capabilities response has no server_id; no archive ref was written"
                    .to_string(),
            )
            .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?
        .to_string();
    let fingerprint = mega2_traces_git::server_fingerprint(&server_id, &authority);
    let fingerprint_key = format!("libra.traces.remote.{}.fingerprint", args.remote);
    let stored_fingerprint =
        crate::command::agent::mega2_stream::repository_config_at(&workdir, &fingerprint_key)
            .await
            .map_err(|error| CliError::fatal(error.to_string()))?;
    mega2_traces_git::fingerprint_conflict(stored_fingerprint.as_deref(), &fingerprint)
        .map_err(argument_error)?;
    let path = format!("/api/v1/traces/streams/{}/archive", args.stream);
    let body = get_json(&endpoint, &path, &[]).await?;
    if let Some(announced) = body.get("stream").and_then(Value::as_str)
        && announced != args.stream
    {
        return Err(CliError::fatal(format!(
            "archive stream {announced} does not match requested stream {}",
            args.stream
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    let hash_kind = match body.get("hash_kind").and_then(Value::as_str) {
        Some("sha1") => HashKind::Sha1,
        Some("sha256") => HashKind::Sha256,
        Some(other) => {
            return Err(CliError::fatal(format!(
                "archive hash kind {other:?} is not sha1 or sha256"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        None => {
            return Err(
                CliError::fatal("archive response has no hash_kind".to_string())
                    .with_stable_code(StableErrorCode::RepoCorrupt),
            );
        }
    };
    let repository_format =
        crate::command::agent::mega2_stream::repository_config_at(&workdir, "core.objectformat")
            .await
            .map_err(|error| CliError::fatal(error.to_string()))?;
    let repository_kind = match repository_format.as_deref().map(str::trim) {
        None | Some("") | Some("sha1") => HashKind::Sha1,
        Some("sha256") => HashKind::Sha256,
        Some(other) => {
            return Err(CliError::fatal(format!(
                "repository object format {other:?} cannot store a traces archive"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
    };
    mega2_traces_git::reject_hash_conversion(hash_kind, repository_kind).map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let tip = body
        .get("tip")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::fatal("archive response has no tip".to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?
        .to_string();
    let objects = parse_archive_objects(body.get("objects"))?;
    mega2_traces_git::validate_archive(&objects, &tip, hash_kind, &args.stream).map_err(
        |message| CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt),
    )?;
    let ref_name = mega2_traces_git::archive_tracking_ref(&args.remote, &args.stream, hash_kind)
        .map_err(argument_error)?;
    if mega2_traces_git::forbids_local_traces_ref(&ref_name) {
        return Err(CliError::fatal(
            "refusing to publish a fetch onto refs/heads/traces or refs/libra/traces".to_string(),
        ));
    }
    publish_archive_ref(&ref_name, &tip).await?;
    record_config(
        &format!("libra.traces.archive.{}.ref", args.stream),
        &ref_name,
    )
    .await?;
    record_config(&format!("libra.traces.archive.{}.tip", args.stream), &tip).await?;
    record_config(
        &format!("libra.traces.archive.{}.hash_kind", args.stream),
        hash_kind.as_str(),
    )
    .await?;
    record_config(&fingerprint_key, &fingerprint).await?;
    record_config(
        &format!("libra.traces.remote.{}.server_id", args.remote),
        &server_id,
    )
    .await?;
    emit_report(
        "fetch",
        serde_json::json!({
            "ref": ref_name,
            "tip": tip,
            "stream": args.stream,
            "hash_kind": hash_kind.as_str(),
            "server_id": server_id,
        }),
        output,
    )
}

async fn record_config(key: &str, value: &str) -> CliResult<()> {
    crate::internal::config::ConfigKv::set(key, value, false)
        .await
        .map_err(|error| CliError::fatal(format!("cannot record {key}: {error}")))
}

fn parse_archive_objects(value: Option<&Value>) -> CliResult<Vec<ArchiveObject>> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Err(
            CliError::fatal("archive response has no objects".to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt),
        );
    };
    let mut objects = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let kind = item
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind.is_empty() {
            return Err(CliError::fatal("archive object has no kind".to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        let payload = item
            .get("payload")
            .and_then(Value::as_str)
            .unwrap_or("")
            .as_bytes()
            .to_vec();
        let links = item
            .get("links")
            .and_then(Value::as_array)
            .map(|links| {
                links
                    .iter()
                    .filter_map(|link| link.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let scope = item
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string);
        objects.push(ArchiveObject {
            id,
            kind,
            payload,
            links,
            scope,
        });
    }
    Ok(objects)
}

async fn publish_archive_ref(name: &str, tip: &str) -> CliResult<()> {
    let connection = get_db_conn_instance().await;
    let transaction = begin_write_transaction(&connection)
        .await
        .map_err(|error| {
            CliError::fatal(format!("cannot start the archive ref transaction: {error}"))
        })?;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    let inserted = reference::ActiveModel {
        name: Set(Some(name.to_string())),
        kind: Set(ConfigKind::Branch),
        commit: Set(Some(tip.to_string())),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&transaction)
    .await;
    if let Err(error) = inserted {
        let _ = transaction.rollback().await;
        return Err(CliError::fatal(format!(
            "cannot write archive ref '{name}': {error}"
        )));
    }
    transaction
        .commit()
        .await
        .map_err(|error| CliError::fatal(format!("cannot commit archive ref '{name}': {error}")))?;
    Ok(())
}

fn remote_http_origin(remote_url: &str) -> CliResult<String> {
    let url = Url::parse(remote_url).map_err(|_| {
        CliError::fatal(format!(
            "remote URL {remote_url:?} is not an HTTP(S) Mega2 endpoint"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let host = url.host_str().unwrap_or("");
    Ok(format!("{}://{host}{port}", url.scheme()))
}

fn optional_time(value: Option<&str>) -> CliResult<Option<i64>> {
    value
        .map(mega2_traces::parse_rfc3339_unix_seconds)
        .transpose()
        .map_err(argument_error)
}

fn utc_bound(value: Option<&str>) -> CliResult<Option<String>> {
    value
        .map(mega2_traces::normalize_rfc3339_utc)
        .transpose()
        .map_err(argument_error)
}

fn admit_code_commit(value: Option<&str>) -> CliResult<()> {
    value
        .map(mega2_traces::validate_code_commit)
        .transpose()
        .map_err(argument_error)?;
    Ok(())
}

fn argument_error(message: String) -> CliError {
    CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn push_owned(query: &mut Vec<(String, String)>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        query.push((key.to_string(), value));
    }
}

fn borrowed(query: &[(String, String)]) -> Vec<(&str, &str)> {
    query
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

fn count(name: &str, value: Option<u64>, default: u64, max: u64) -> CliResult<u64> {
    mega2_traces::bounded_count(name, value, default, max).map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidArguments)
    })
}

async fn get_json(endpoint: &EndpointArgs, path: &str, query: &[(&str, &str)]) -> CliResult<Value> {
    let server = validate_server_url(&endpoint.server)?;
    let token = resolve_token(
        endpoint.token_file.as_deref(),
        std::env::var(crate::internal::protocol::mega2_auth::ENV_TOKEN_VAR)
            .ok()
            .as_deref(),
        endpoint.token.as_deref(),
    )?;
    let mut url = server
        .join(path.trim_start_matches('/'))
        .map_err(|error| CliError::fatal(format!("cannot build the traces URL: {error}")))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
        drop(pairs);
    }
    let bytes = fetch_bounded(&url, token.as_ref().map(|token| token.expose())).await?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CliError::fatal(format!("traces response is not JSON: {error}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })
}

async fn fetch_bounded(start: &Url, token: Option<&str>) -> CliResult<Vec<u8>> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            CliError::fatal(format!("cannot build the traces HTTP client: {error}"))
        })?;
    let mut url = start.clone();
    let mut authorization = token.map(|token| format!("Bearer {token}"));
    for _ in 0..5 {
        let mut request = client.get(url.clone());
        if let Some(header) = authorization.as_deref() {
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        let response = request.send().await.map_err(|error| {
            CliError::fatal(format!("traces request failed: {error}"))
                .with_stable_code(StableErrorCode::NetworkUnavailable)
        })?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| CliError::fatal("traces redirect has no Location".to_string()))?;
            let next = url.join(location).map_err(|error| {
                CliError::fatal(format!("traces redirect location is invalid: {error}"))
            })?;
            authorization =
                mega2_traces::bearer_after_redirect(&url, &next, authorization.as_deref());
            url = next;
            continue;
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                CliError::fatal(format!("traces response read failed: {error}"))
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
            })?;
            if body.len().saturating_add(chunk.len()) > RESPONSE_BYTES_MAX {
                return Err(CliError::fatal(format!(
                    "traces response exceeds the {RESPONSE_BYTES_MAX} byte limit"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(map_http_status(status.as_u16(), &body));
        }
        return Ok(body);
    }
    Err(
        CliError::fatal("traces request followed too many redirects".to_string())
            .with_stable_code(StableErrorCode::NetworkUnavailable),
    )
}

fn map_http_status(status: u16, body: &[u8]) -> CliError {
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let message = if code.is_empty() {
        format!("traces server returned HTTP {status}")
    } else {
        format!("traces server returned HTTP {status} {code}")
    };
    let error = CliError::fatal(message);
    match status {
        401 => error.with_stable_code(StableErrorCode::AuthMissingCredentials),
        404 => error.with_stable_code(StableErrorCode::RepoNotFound),
        409 | 413 => error.with_stable_code(StableErrorCode::ConflictOperationBlocked),
        503 => error.with_stable_code(StableErrorCode::NetworkUnavailable),
        _ => error.with_stable_code(StableErrorCode::CliInvalidArguments),
    }
}

fn emit_report(operation: &'static str, body: Value, output: &OutputConfig) -> CliResult<()> {
    crate::utils::output::emit(&TracesReport { operation, body }, output)
}
