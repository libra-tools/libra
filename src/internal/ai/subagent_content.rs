//! Source-scoped subagent transcript capture (plan-20260713 M5 / DR-06).
//!
//! Provider files are discovered beneath a held, no-follow directory and are
//! converted to typed, allowlist-only projections before persistence.  The
//! checkpoint catalog row, source revision/current leaf, association row, and
//! traces ref CAS commit atomically through [`history::TracesTxnExtra`].

#[cfg(unix)]
use std::process::Stdio;
use std::{
    collections::HashMap,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use tokio::io::AsyncWriteExt;

use super::{
    history::{
        self, CheckpointCommitParams, CheckpointScope, HistoryManager, TracesCommitCtx,
        TracesInflightMarker, TracesTxnExtra,
    },
    observed_agents::{
        ClaudeCodeObservedAgent, RedactedBytes, Redactor, TRANSCRIPT_READ_HARD_CAP_BYTES,
        claude_project_slug, claude_session_dir, claude_session_id_is_safe_path_component,
        normalize_claude_transcript, normalize_claude_transcript_until,
        open_file_beneath_pinned_provider_directory, open_provider_directory_for_discovery,
        parse_canon_value, pinned_provider_directory_path, redact_turns_with_report,
        safe_turn_projection,
    },
};
use crate::utils::client_storage::ClientStorage;

pub const SUBAGENT_CONTENT_SCHEMA_VERSION: i64 = 1;
pub const SUBAGENT_DISCOVERY_HELPER_ARG: &str = "--libra-internal-agent-subagent-discovery-helper";
pub const SUBAGENT_DISCOVERY_HELPER_INPUT_CAP: u64 = 1024 * 1024;
pub const SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP: u64 = 24 * 1024 * 1024;
const SUBAGENT_CONTENT_LEASE_MS: i64 = 60_000;
const MAX_SUBAGENT_DIRECTORY_ENTRIES: usize = 2_048;
pub(crate) const MAX_SUBAGENT_SOURCES_PER_CAPTURE: usize = 16;
const SUBAGENT_PARENT_PERSISTENCE_RESERVE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
#[error("subagent discovery exhausted its parent-preservation deadline")]
pub(crate) struct SubagentDiscoveryDeadline;

/// One provider-authorized child transcript. Fields remain private so callers
/// cannot forge the discovery proof with arbitrary bytes.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredSubagentContent {
    provider_kind: String,
    source_key: String,
    bytes: Vec<u8>,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
}

/// Bounded discovery result.  `bytes_read` is charged to the historical
/// import command's cumulative input budget even if later persistence fails.
#[derive(Debug, Clone, Default)]
pub(crate) struct SubagentDiscovery {
    pub sources: Vec<DiscoveredSubagentContent>,
    pub bytes_read: u64,
    /// A platform capability warning is non-fatal for the parent checkpoint.
    /// It is diagnostic only: absence of a supported discovery mechanism does
    /// not prove that child evidence exists or is incomplete.
    pub warning: Option<String>,
    /// Child evidence could not be validated within its reserved slice of the
    /// command deadline. The independently valid parent remains importable,
    /// but its result must be reported as partial.
    pub incomplete: bool,
}

impl SubagentDiscovery {
    pub(crate) fn partial_source_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.partial)
            .count()
            .saturating_add(usize::from(self.incomplete))
    }

    pub(crate) fn from_deadline_error(error: &anyhow::Error) -> Option<Self> {
        error
            .downcast_ref::<SubagentDiscoveryDeadline>()
            .map(|_| Self {
                warning: Some(
                    "subagent discovery exceeded its reserved time; parent evidence was preserved as partial"
                        .to_string(),
                ),
                incomplete: true,
                ..Self::default()
            })
    }
}

impl DiscoveredSubagentContent {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    fn fixture(
        provider_kind: &str,
        source_key: &str,
        bytes: &[u8],
        stable_subagent_id: Option<&str>,
    ) -> Self {
        Self {
            provider_kind: provider_kind.to_string(),
            source_key: source_key.to_string(),
            bytes: bytes.to_vec(),
            malformed_lines: 0,
            partial: false,
            stable_subagent_id: stable_subagent_id.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentCaptureSummary {
    pub discovered: usize,
    pub checkpoints_written: usize,
    pub skipped_unchanged: usize,
    pub skipped_inflight: usize,
    pub partial_sources: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("subagent content capture failed after durable progress: {source:#}")]
pub(crate) struct SubagentCaptureProgressError {
    summary: SubagentCaptureSummary,
    #[source]
    source: anyhow::Error,
}

impl SubagentCaptureProgressError {
    pub(crate) fn summary(&self) -> &SubagentCaptureSummary {
        &self.summary
    }
}

fn ensure_before_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        bail!("subagent content capture exceeded its command deadline");
    }
    Ok(())
}

fn ensure_before_discovery_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(SubagentDiscoveryDeadline.into());
    }
    Ok(())
}

/// Reserve enough of the caller's absolute deadline to persist the parent
/// checkpoint when child discovery or parent-side validation is slow.
pub(crate) fn discovery_deadline_preserving_parent(command_deadline: Instant) -> Result<Instant> {
    let now = Instant::now();
    if now >= command_deadline {
        return Err(SubagentDiscoveryDeadline.into());
    }
    let remaining = command_deadline.saturating_duration_since(now);
    let reserve = SUBAGENT_PARENT_PERSISTENCE_RESERVE.min(remaining / 2);
    now.checked_add(remaining.saturating_sub(reserve))
        .context("compute parent-preserving subagent discovery deadline")
}

async fn subagent_discovery_test_pause_before_parent_validation() {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("LIBRA_TEST_SUBAGENT_PARENT_VALIDATION_DELAY_MS")
        && let Ok(delay_ms) = value.parse::<u64>()
        && delay_ms > 0
    {
        tokio::time::sleep(Duration::from_millis(delay_ms.min(30_000))).await;
    }
}

fn subagent_source_completeness(bytes: &[u8], deadline: Option<Instant>) -> Result<(usize, bool)> {
    let mut malformed_lines = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        ensure_before_deadline(deadline)?;
        if !line.iter().all(u8::is_ascii_whitespace) && parse_canon_value(line).is_err() {
            malformed_lines = malformed_lines.saturating_add(1);
        }
    }
    let turns = match deadline {
        Some(deadline) => normalize_claude_transcript_until(bytes, deadline)
            .context("subagent content normalization exceeded its command deadline")?,
        None => normalize_claude_transcript(bytes),
    };
    ensure_before_deadline(deadline)?;
    let partial = malformed_lines > 0
        || turns.is_empty()
        || turns
            .iter()
            .any(|turn| turn.completeness.as_db_str() == "incomplete");
    Ok((malformed_lines, partial))
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_CONTENT_FAILPOINT: Option<&'static str>;
}

fn subagent_content_test_failpoint(_stage: &str) -> Result<()> {
    #[cfg(test)]
    if TEST_SUBAGENT_CONTENT_FAILPOINT
        .try_with(|configured| {
            configured.is_some_and(|configured| {
                configured.split(',').any(|configured| configured == _stage)
            })
        })
        .unwrap_or(false)
    {
        bail!("injected subagent content failure at {_stage}");
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct SubagentDiscoveryHelperRequest {
    cwd_base64: String,
    provider_session_id: String,
    remaining_ms: u64,
    byte_budget: u64,
    source_limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveredSubagentContentWire {
    provider_kind: String,
    source_key: String,
    bytes_base64: String,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SubagentDiscoveryHelperResponse {
    Ok {
        sources: Vec<DiscoveredSubagentContentWire>,
        bytes_read: u64,
        warning: Option<String>,
        incomplete: bool,
    },
    Error {
        message: String,
        deadline_exceeded: bool,
    },
}

#[cfg(unix)]
fn encode_helper_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    STANDARD.encode(path.as_os_str().as_bytes())
}

#[cfg(unix)]
fn decode_helper_path(encoded: &str) -> Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let bytes = STANDARD
        .decode(encoded)
        .context("decode subagent discovery helper path")?;
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn helper_program_from(current: &Path) -> Option<PathBuf> {
    let current_name = current.file_stem().and_then(|name| name.to_str());
    if current_name == Some("libra") {
        return Some(current.to_path_buf());
    }
    let debug_dir = current.parent().and_then(Path::parent)?;
    let mut candidate = debug_dir.join("libra");
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        candidate.set_extension(std::env::consts::EXE_EXTENSION);
    }
    candidate.is_file().then_some(candidate)
}

fn helper_program() -> Result<Option<PathBuf>> {
    let current = std::env::current_exe().context("resolve subagent discovery helper")?;
    Ok(helper_program_from(&current))
}

fn helper_unavailable_discovery() -> SubagentDiscovery {
    SubagentDiscovery {
        warning: Some(
            "killable subagent content discovery is unavailable in this executable".to_string(),
        ),
        ..SubagentDiscovery::default()
    }
}

/// Private helper entry: performs every potentially blocking provider-filesystem
/// operation outside the hook/import Tokio process. The parent owns the actual
/// kill deadline; this inner deadline also keeps ordinary local work bounded.
#[doc(hidden)]
pub fn run_subagent_discovery_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: SubagentDiscoveryHelperRequest =
        serde_json::from_slice(input).context("decode subagent discovery helper request")?;
    #[cfg(not(unix))]
    let _ = &request;
    #[cfg(not(unix))]
    let response = SubagentDiscoveryHelperResponse::Ok {
        sources: Vec::new(),
        bytes_read: 0,
        warning: Some("secure subagent content discovery is unavailable on this platform".into()),
        incomplete: false,
    };
    #[cfg(unix)]
    let response = {
        let cwd = decode_helper_path(&request.cwd_base64)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(request.remaining_ms.max(1)))
            .context("compute subagent discovery helper deadline")?;
        match discover_claude_subagent_contents(
            &cwd,
            &request.provider_session_id,
            Some(deadline),
            request.byte_budget,
            request.source_limit,
        ) {
            Ok(discovery) => SubagentDiscoveryHelperResponse::Ok {
                sources: discovery
                    .sources
                    .into_iter()
                    .map(|source| DiscoveredSubagentContentWire {
                        provider_kind: source.provider_kind,
                        source_key: source.source_key,
                        bytes_base64: STANDARD.encode(source.bytes),
                        malformed_lines: source.malformed_lines,
                        partial: source.partial,
                        stable_subagent_id: source.stable_subagent_id,
                    })
                    .collect(),
                bytes_read: discovery.bytes_read,
                warning: discovery.warning,
                incomplete: discovery.incomplete,
            },
            Err(error) => SubagentDiscoveryHelperResponse::Error {
                message: format!("{error:#}"),
                deadline_exceeded: Instant::now() >= deadline,
            },
        }
    };
    serde_json::to_vec(&response).context("encode subagent discovery helper response")
}

/// Killable async boundary used by every production live/import caller.
pub(crate) async fn discover_claude_subagent_contents_bounded(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Instant,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    #[cfg(not(unix))]
    return discover_claude_subagent_contents(
        cwd,
        provider_session_id,
        Some(deadline),
        byte_budget,
        source_limit,
    );

    #[cfg(unix)]
    {
        ensure_before_discovery_deadline(deadline)?;
        let Some(helper_program) = helper_program()? else {
            // Embedders and renamed executables may not have a sibling Libra
            // helper. Fail closed for child content: blocking provider files
            // must never be opened in the async host process because they
            // cannot be killed when the command deadline expires.
            return Ok(helper_unavailable_discovery());
        };
        let remaining_ms = u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        let request = SubagentDiscoveryHelperRequest {
            cwd_base64: encode_helper_path(cwd),
            provider_session_id: provider_session_id.to_string(),
            remaining_ms,
            byte_budget,
            source_limit,
        };
        let frame =
            serde_json::to_vec(&request).context("encode bounded subagent discovery request")?;
        if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_INPUT_CAP {
            bail!("bounded subagent discovery request exceeds its internal frame limit");
        }
        let mut child = tokio::process::Command::new(helper_program)
            .arg(SUBAGENT_DISCOVERY_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start bounded subagent discovery helper")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("bounded subagent discovery helper has no stdin pipe"))?;
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            stdin.write_all(&frame).await?;
            stdin.shutdown().await
        })
        .await
        .map_err(|_| SubagentDiscoveryDeadline)?
        .context("send bounded subagent discovery request")?;
        drop(stdin);
        let output = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| SubagentDiscoveryDeadline)?
        .context("wait for bounded subagent discovery helper")?;
        if !output.status.success()
            || output.stdout.len() as u64 > SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
        {
            bail!("bounded subagent discovery helper returned an invalid response");
        }
        subagent_discovery_test_pause_before_parent_validation().await;
        ensure_before_discovery_deadline(deadline)?;
        let response = serde_json::from_slice(&output.stdout)
            .context("decode bounded subagent discovery response")?;
        ensure_before_discovery_deadline(deadline)?;
        match response {
            SubagentDiscoveryHelperResponse::Ok {
                sources,
                bytes_read,
                warning,
                incomplete,
            } => {
                if sources.len() > source_limit
                    || warning.as_ref().is_some_and(|value| value.len() > 4_096)
                {
                    bail!("bounded subagent discovery helper exceeded its response limits");
                }
                let mut decoded = Vec::with_capacity(sources.len());
                let mut decoded_bytes = 0_u64;
                for source in sources {
                    ensure_before_discovery_deadline(deadline)?;
                    if source.provider_kind != "claude_code" || source.stable_subagent_id.is_some()
                    {
                        bail!(
                            "bounded subagent discovery helper returned invalid Claude attribution"
                        );
                    }
                    validate_source_identity(&source.provider_kind, &source.source_key)?;
                    let bytes = STANDARD
                        .decode(source.bytes_base64)
                        .context("decode bounded subagent transcript bytes")?;
                    ensure_before_discovery_deadline(deadline)?;
                    decoded_bytes = decoded_bytes
                        .checked_add(u64::try_from(bytes.len()).map_err(|error| {
                            anyhow!("subagent transcript size overflow: {error}")
                        })?)
                        .context("bounded subagent transcript byte count overflow")?;
                    let completeness = subagent_source_completeness(&bytes, Some(deadline));
                    let (malformed_lines, partial) = match completeness {
                        Ok(completeness) => completeness,
                        Err(_) if Instant::now() >= deadline => {
                            return Err(SubagentDiscoveryDeadline.into());
                        }
                        Err(error) => return Err(error),
                    };
                    if source.malformed_lines != malformed_lines || source.partial != partial {
                        bail!(
                            "bounded subagent discovery helper returned inconsistent completeness metadata"
                        );
                    }
                    decoded.push(DiscoveredSubagentContent {
                        provider_kind: source.provider_kind,
                        source_key: source.source_key,
                        bytes,
                        malformed_lines,
                        partial,
                        stable_subagent_id: None,
                    });
                }
                if decoded_bytes != bytes_read || decoded_bytes > byte_budget {
                    bail!("bounded subagent discovery helper returned an inconsistent byte count");
                }
                ensure_before_discovery_deadline(deadline)?;
                Ok(SubagentDiscovery {
                    sources: decoded,
                    bytes_read,
                    warning,
                    incomplete,
                })
            }
            SubagentDiscoveryHelperResponse::Error {
                message,
                deadline_exceeded,
            } => {
                if deadline_exceeded {
                    return Err(SubagentDiscoveryDeadline.into());
                }
                bail!("bounded subagent discovery failed: {message}")
            }
        }
    }
}

fn validate_source_identity(provider_kind: &str, source_key: &str) -> Result<()> {
    if provider_kind.is_empty() || provider_kind.len() > 64 {
        bail!("subagent provider kind is empty or exceeds 64 bytes");
    }
    if source_key.is_empty() || source_key.len() > 4_096 {
        bail!("subagent source key is empty or exceeds 4096 bytes");
    }
    let path = Path::new(source_key);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("subagent source key must be provider-root-relative and normalized");
    }
    Ok(())
}

fn source_key_for_incarnation(source_key: &str, namespace: Option<&str>) -> Result<String> {
    let Some(namespace) = namespace else {
        return Ok(source_key.to_string());
    };
    if namespace.len() != 32 || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("agent capture incarnation namespace is invalid; run `libra agent doctor`");
    }
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-source-incarnation-v1");
    for value in [namespace.as_bytes(), source_key.as_bytes()] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    Ok(format!("source/sha256/{}", hex::encode(digest.finalize())))
}

/// Discover Claude's `<project>/<session>/subagents/*.jsonl` sources while
/// holding the directory inode and opening every file via `openat` +
/// `O_NOFOLLOW`. Persistence receives only a SHA-256 digest of the authorized
/// provider-root-relative identity, never its local project slug or filename.
pub(crate) fn discover_claude_subagent_contents(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Option<Instant>,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    // Historical imports may carry legitimate provider identifiers from old
    // Claude versions that do not satisfy the current hex/dash disk naming
    // contract. Such an id is not safe to interpolate into a provider path,
    // so content discovery is unavailable for it while the parent import
    // remains backward-compatible.
    if !claude_session_id_is_safe_path_component(provider_session_id) {
        return Ok(SubagentDiscovery {
            warning: Some(
                "subagent content discovery skipped an unsafe legacy provider session id"
                    .to_string(),
            ),
            ..SubagentDiscovery::default()
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (cwd, deadline, byte_budget, source_limit);
        return Ok(SubagentDiscovery {
            warning: Some(
                "secure subagent content discovery is unavailable on this platform".to_string(),
            ),
            ..SubagentDiscovery::default()
        });
    }
    #[cfg(unix)]
    let result = (|| -> Result<SubagentDiscovery> {
        ensure_before_deadline(deadline)?;
        let Some(project_dir) = claude_session_dir(cwd) else {
            return Ok(SubagentDiscovery::default());
        };
        let subagents_dir = project_dir.join(provider_session_id).join("subagents");
        let adapter = ClaudeCodeObservedAgent::new();
        let Some(directory) = open_provider_directory_for_discovery(&adapter, &subagents_dir)?
        else {
            return Ok(SubagentDiscovery::default());
        };
        let pinned_path = pinned_provider_directory_path(&directory);
        if pinned_path.as_os_str().is_empty() {
            bail!("secure provider directory enumeration is unavailable on this platform");
        }

        let mut names = Vec::new();
        let mut entry_count = 0usize;
        for entry in
            std::fs::read_dir(&pinned_path).context("enumerate pinned Claude subagent directory")?
        {
            ensure_before_deadline(deadline)?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_SUBAGENT_DIRECTORY_ENTRIES {
                bail!(
                    "Claude subagent directory exceeds {} entry safety limit",
                    MAX_SUBAGENT_DIRECTORY_ENTRIES
                );
            }
            let entry = entry.context("read pinned Claude subagent directory entry")?;
            let file_type = entry
                .file_type()
                .context("inspect Claude subagent directory entry")?;
            if file_type.is_symlink() {
                bail!("refusing symlink in Claude subagent directory (fail-closed)");
            }
            let name = entry.file_name();
            if Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                != Some("jsonl")
            {
                continue;
            }
            if !file_type.is_file() {
                bail!("refusing non-regular Claude subagent JSONL source");
            }
            if names.len() >= source_limit {
                bail!("Claude subagent directory exceeds {source_limit} source capture limit");
            }
            names.push(name);
        }
        names.sort();

        let mut total_bytes = 0u64;
        let mut discovered = Vec::with_capacity(names.len());
        for name in names {
            ensure_before_deadline(deadline)?;
            let name_text = name
                .to_str()
                .context("Claude subagent source name is not valid UTF-8")?;
            let mut file =
                open_file_beneath_pinned_provider_directory(&directory, Path::new(name_text))?;
            let length = file
                .metadata()
                .context("inspect securely opened Claude subagent source")?
                .len();
            let effective_budget = byte_budget.min(TRANSCRIPT_READ_HARD_CAP_BYTES);
            if length > effective_budget || total_bytes.saturating_add(length) > effective_budget {
                bail!(
                    "Claude subagent transcript set exceeds {effective_budget} byte input budget"
                );
            }
            let remaining = effective_budget.saturating_sub(total_bytes);
            let mut bytes = Vec::new();
            file.by_ref()
                .take(remaining.saturating_add(1))
                .read_to_end(&mut bytes)
                .context("read securely opened Claude subagent source")?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > remaining {
                bail!(
                    "Claude subagent transcript set grew beyond {effective_budget} byte input budget while reading"
                );
            }
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            // Compute the completeness bit inside the killable discovery
            // helper too; callers need it before import identity finalization.
            let (malformed_lines, partial) = subagent_source_completeness(&bytes, deadline)?;
            let authorized_relative_source = format!(
                "{}/{provider_session_id}/subagents/{name_text}",
                claude_project_slug(cwd)
            );
            // Persist only an opaque digest of the provider-root-relative source.
            // The raw slug/filename remains an in-memory authorization input and
            // cannot leak usernames or customer names into SQLite/traces metadata.
            let source_key = format!(
                "source/sha256/{}",
                hex::encode(Sha256::digest(authorized_relative_source.as_bytes()))
            );
            validate_source_identity("claude_code", &source_key)?;
            discovered.push(DiscoveredSubagentContent {
                provider_kind: "claude_code".to_string(),
                source_key,
                bytes,
                malformed_lines,
                partial,
                // Claude's disk filename is not a provider guarantee that also
                // appears on hook boundaries, so guessing from it is forbidden.
                stable_subagent_id: None,
            });
        }
        Ok(SubagentDiscovery {
            sources: discovered,
            bytes_read: total_bytes,
            warning: None,
            incomplete: false,
        })
    })();
    #[cfg(unix)]
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationOutcome {
    Reserved { fence_token: i64, revision: i64 },
    Unchanged { checkpoint_exists: bool },
    Inflight { lease_expires_at: i64 },
    DurabilityProofStale,
}

struct ReservationAttempt<'a> {
    content_digest: &'a str,
    checkpoint_id: &'a str,
    owner: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DurableCheckpointIdentity {
    traces_commit: String,
    tree_oid: String,
    metadata_blob_oid: String,
}

#[derive(Default)]
struct UnchangedDurabilityProof {
    traces_head: Option<String>,
    checkpoints: HashMap<String, DurableCheckpointIdentity>,
    source_checkpoints: HashMap<(String, String, String), String>,
}

impl UnchangedDurabilityProof {
    fn source_checkpoint(
        &self,
        source: &DiscoveredSubagentContent,
        content_digest: &str,
    ) -> Option<&str> {
        self.source_checkpoints
            .get(&(
                source.provider_kind.clone(),
                source.source_key.clone(),
                content_digest.to_string(),
            ))
            .map(String::as_str)
    }
}

async fn reserve_source(
    conn: &DatabaseConnection,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    attempt: ReservationAttempt<'_>,
    durability_proof: &UnchangedDurabilityProof,
) -> Result<ReservationOutcome> {
    let ReservationAttempt {
        content_digest,
        checkpoint_id,
        owner,
    } = attempt;
    let now_ms = Utc::now().timestamp_millis();
    let txn = conn
        .begin()
        .await
        .context("begin subagent content reservation")?;
    let lease_expires_at = now_ms.saturating_add(SUBAGENT_CONTENT_LEASE_MS);
    let inserted = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                current_revision, current_checkpoint_id, current_digest, state,
                attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, NULL, NULL, 'reserved', ?, ?, ?, ?, 1, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key, content_schema_version)
             DO NOTHING",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                content_digest.into(),
                checkpoint_id.into(),
                owner.into(),
                lease_expires_at.into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("insert initial subagent content reservation")?;
    if inserted.rows_affected() == 1 {
        txn.commit()
            .await
            .context("commit initial subagent content reservation")?;
        return Ok(ReservationOutcome::Reserved {
            fence_token: 1,
            revision: 1,
        });
    }

    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT revision_cursor, current_revision, current_checkpoint_id, current_digest, state,
                    attempt_digest, lease_expires_at, fence_token
             FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read existing subagent content claim")?
        .context("subagent content claim disappeared during reservation")?;
    let revision_cursor: i64 = row.try_get_by("revision_cursor")?;
    let current_revision: i64 = row.try_get_by("current_revision")?;
    let current_checkpoint_id: Option<String> = row.try_get_by("current_checkpoint_id")?;
    let current_digest: Option<String> = row.try_get_by("current_digest")?;
    let state: String = row.try_get_by("state")?;
    let attempt_digest: Option<String> = row.try_get_by("attempt_digest")?;
    let current_lease: Option<i64> = row.try_get_by("lease_expires_at")?;
    let fence_token: i64 = row.try_get_by("fence_token")?;
    if state == "reserved" && current_lease.is_some_and(|lease| lease > now_ms) {
        let lease_expires_at = current_lease.unwrap_or(now_ms);
        txn.commit()
            .await
            .context("commit in-flight subagent content probe")?;
        tracing::debug!(
            same_digest = attempt_digest.as_deref() == Some(content_digest),
            "subagent content source is owned by another live writer"
        );
        return Ok(ReservationOutcome::Inflight { lease_expires_at });
    }
    if state == "reserved" && current_digest.as_deref() == Some(content_digest) {
        let cleared = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_subagent_content_claim
                 SET state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                     owner = NULL, lease_expires_at = NULL, fence_token = fence_token + 1,
                     updated_at = ?
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'reserved'
                   AND fence_token = ? AND lease_expires_at <= ?",
                [
                    now_ms.into(),
                    parent_session_id.into(),
                    source.provider_kind.clone().into(),
                    source.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    fence_token.into(),
                    now_ms.into(),
                ],
            ))
            .await
            .context("clear expired subagent content reservation")?;
        if cleared.rows_affected() != 1 {
            txn.rollback()
                .await
                .context("roll back lost expired subagent reservation cleanup")?;
            return Ok(ReservationOutcome::Inflight {
                lease_expires_at: now_ms.saturating_add(25),
            });
        }
    }
    if current_digest.as_deref() == Some(content_digest) {
        let current_checkpoint_id = current_checkpoint_id.as_deref().context(
            "subagent content claim has a digest but no current checkpoint; run `libra agent doctor`",
        )?;
        let intact = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT c.traces_commit, c.tree_oid, c.metadata_blob_oid
                 FROM agent_checkpoint c
                 JOIN agent_subagent_content_revision r
                   ON r.checkpoint_id = c.checkpoint_id
                  AND r.parent_session_id = ?
                  AND r.provider_kind = ?
                  AND r.source_key = ?
                  AND r.content_schema_version = ?
                  AND r.revision = ?
                  AND r.content_digest = ?
                 JOIN agent_subagent_link l
                   ON l.content_checkpoint_id = c.checkpoint_id
                  AND l.parent_session_id = ?
                 WHERE c.checkpoint_id = ? AND c.session_id = ? AND c.scope = 'subagent'",
                [
                    parent_session_id.into(),
                    source.provider_kind.clone().into(),
                    source.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    current_revision.into(),
                    content_digest.into(),
                    parent_session_id.into(),
                    current_checkpoint_id.into(),
                    parent_session_id.into(),
                ],
            ))
            .await
            .context("verify unchanged subagent content relation")?;
        if intact.is_none() {
            bail!(
                "subagent content current leaf is incomplete; run `libra agent doctor` before replaying this source"
            );
        }
        let intact = intact.context("unchanged subagent content relation disappeared")?;
        let traces_commit: String = intact.try_get_by("traces_commit")?;
        let tree_oid: String = intact.try_get_by("tree_oid")?;
        let metadata_blob_oid: String = intact.try_get_by("metadata_blob_oid")?;
        let expected = DurableCheckpointIdentity {
            traces_commit,
            tree_oid,
            metadata_blob_oid,
        };
        let proven_checkpoint = durability_proof.source_checkpoint(source, content_digest);
        if proven_checkpoint != Some(current_checkpoint_id)
            || durability_proof.checkpoints.get(current_checkpoint_id) != Some(&expected)
        {
            txn.commit()
                .await
                .context("commit stale subagent durability proof probe")?;
            return Ok(ReservationOutcome::DurabilityProofStale);
        }
        let current_head = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT `commit` FROM reference
                 WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
                [crate::internal::branch::TRACES_BRANCH.into()],
            ))
            .await
            .context("recheck traces head for unchanged subagent content")?
            .map(|row| row.try_get_by::<Option<String>, _>("commit"))
            .transpose()?
            .flatten();
        if current_head != durability_proof.traces_head {
            txn.commit()
                .await
                .context("commit changed traces-head durability probe")?;
            return Ok(ReservationOutcome::DurabilityProofStale);
        }
        txn.commit()
            .await
            .context("commit unchanged subagent content probe")?;
        return Ok(ReservationOutcome::Unchanged {
            checkpoint_exists: true,
        });
    }
    let updated = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'reserved', attempt_digest = ?, attempt_checkpoint_id = ?,
                 owner = ?, lease_expires_at = ?, fence_token = fence_token + 1,
                 updated_at = ?
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?
               AND (state = 'idle' OR lease_expires_at <= ?)",
            [
                content_digest.into(),
                checkpoint_id.into(),
                owner.into(),
                lease_expires_at.into(),
                now_ms.into(),
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("take over subagent content reservation")?;
    if updated.rows_affected() != 1 {
        txn.rollback()
            .await
            .context("roll back lost subagent content reservation")?;
        return Ok(ReservationOutcome::Inflight {
            lease_expires_at: now_ms.saturating_add(25),
        });
    }
    let fence_row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT fence_token FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read subagent content reservation fence")?
        .context("subagent content claim disappeared after takeover")?;
    let fence_token: i64 = fence_row.try_get_by("fence_token")?;
    txn.commit()
        .await
        .context("commit subagent content reservation")?;
    Ok(ReservationOutcome::Reserved {
        fence_token,
        revision: revision_cursor.saturating_add(1),
    })
}

async fn release_reservation(
    conn: &DatabaseConnection,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    checkpoint_id: &str,
    owner: &str,
    fence_token: i64,
) -> Result<()> {
    subagent_content_test_failpoint("before_release_reservation")?;
    let released = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                 owner = NULL, lease_expires_at = NULL, updated_at = ?
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ? AND state = 'reserved'
               AND attempt_checkpoint_id = ? AND owner = ? AND fence_token = ?",
            [
                Utc::now().timestamp_millis().into(),
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                checkpoint_id.into(),
                owner.into(),
                fence_token.into(),
            ],
        ))
        .await
        .with_context(|| {
            format!("release failed subagent content reservation for checkpoint {checkpoint_id}")
        })?;
    if released.rows_affected() != 1 {
        bail!(
            "failed to release subagent content reservation for checkpoint {checkpoint_id}: \
             its ownership fence changed; run `libra agent doctor` before retrying"
        );
    }
    Ok(())
}

fn preserve_cleanup_error(
    primary: anyhow::Error,
    cleanup_operation: &str,
    cleanup: anyhow::Error,
) -> anyhow::Error {
    primary.context(format!(
        "{cleanup_operation} also failed: {cleanup:#}; run `libra agent doctor` before retrying"
    ))
}

async fn unique_boundary_checkpoint<C: ConnectionTrait>(
    conn: &C,
    parent_session_id: &str,
    stable_subagent_id: Option<&str>,
) -> Result<Option<String>> {
    let Some(stable_id) = stable_subagent_id else {
        return Ok(None);
    };
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint
             WHERE session_id = ? AND scope = 'subagent'
               AND (subagent_session_id = ? OR tool_use_id = ?)
             ORDER BY created_at, checkpoint_id LIMIT 2",
            [parent_session_id.into(), stable_id.into(), stable_id.into()],
        ))
        .await
        .context("resolve stable subagent boundary association")?;
    if rows.len() != 1 {
        return Ok(None);
    }
    rows.into_iter()
        .next()
        .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
        .transpose()
        .context("read unique subagent boundary checkpoint")
}

async fn refresh_current_link(
    conn: &DatabaseConnection,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
) -> Result<()> {
    let Some(stable_id) = source.stable_subagent_id.as_deref() else {
        return Ok(());
    };
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .context("begin subagent link refresh")?;
    let current = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT current_checkpoint_id FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ? AND current_revision > 0",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read current subagent checkpoint for link refresh")?;
    let Some(current) = current else {
        txn.commit()
            .await
            .context("commit empty subagent link refresh")?;
        return Ok(());
    };
    let checkpoint_id: Option<String> = current.try_get_by("current_checkpoint_id")?;
    let Some(checkpoint_id) = checkpoint_id else {
        txn.commit()
            .await
            .context("commit unmaterialized subagent link refresh")?;
        return Ok(());
    };
    let boundary = unique_boundary_checkpoint(&txn, parent_session_id, Some(stable_id)).await?;
    let Some(boundary) = boundary else {
        txn.commit()
            .await
            .context("commit unresolved subagent link refresh")?;
        return Ok(());
    };
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE agent_subagent_link
         SET link_state = 'resolved', boundary_checkpoint_id = ?,
             sync_revision = sync_revision + 1, updated_at = ?
         WHERE content_checkpoint_id = ? AND parent_session_id = ?
           AND stable_subagent_id = ? AND link_state = 'unresolved'",
        [
            boundary.into(),
            Utc::now().timestamp_millis().into(),
            checkpoint_id.into(),
            parent_session_id.into(),
            stable_id.into(),
        ],
    ))
    .await
    .context("resolve subagent content link without rewriting checkpoint history")?;
    txn.commit().await.context("commit subagent link refresh")?;
    Ok(())
}

#[derive(Debug)]
struct ContentCommitPlan {
    parent_session_id: String,
    provider_kind: String,
    source_key: String,
    content_digest: String,
    checkpoint_id: String,
    owner: String,
    fence_token: i64,
    revision: i64,
    parent_commit: Option<String>,
    source_channel: String,
    partial: bool,
    stable_subagent_id: Option<String>,
    created_at: i64,
}

#[async_trait::async_trait]
impl TracesTxnExtra for ContentCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        subagent_content_test_failpoint("before_final_sql")?;
        let writable = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
                [self.parent_session_id.clone().into()],
            ))
            .await
            .context("verify subagent content tombstone write barrier")?;
        if writable.is_none() {
            bail!("parent agent session was erased while subagent content was in flight");
        }

        let reservation = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT revision_cursor FROM agent_subagent_content_claim
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'reserved'
                   AND attempt_digest = ? AND attempt_checkpoint_id = ?
                   AND owner = ? AND fence_token = ?",
                [
                    self.parent_session_id.clone().into(),
                    self.provider_kind.clone().into(),
                    self.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    self.content_digest.clone().into(),
                    self.checkpoint_id.clone().into(),
                    self.owner.clone().into(),
                    self.fence_token.into(),
                ],
            ))
            .await
            .context("verify subagent content reservation fence")?
            .context("subagent content reservation was lost before final commit")?;
        let revision_cursor: i64 = reservation.try_get_by("revision_cursor")?;
        if self.revision != revision_cursor.saturating_add(1) {
            bail!("subagent content revision changed while writer was in flight");
        }

        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, ?, NULL, 'subagent', ?, ?, ?, ?, NULL, ?, ?, ?)",
            [
                self.checkpoint_id.clone().into(),
                self.parent_session_id.clone().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                // Content provenance keeps the stable id in the association
                // table. This catalog column is reserved for boundary
                // evidence; populating it here would let content match itself.
                Option::<String>::None.into(),
                "subagent content".into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("insert subagent content checkpoint catalog row")?;

        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_revision (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision, checkpoint_id, content_digest, source_channel, partial, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                self.parent_session_id.clone().into(),
                self.provider_kind.clone().into(),
                self.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                self.revision.into(),
                self.checkpoint_id.clone().into(),
                self.content_digest.clone().into(),
                self.source_channel.clone().into(),
                i64::from(self.partial).into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("append subagent content source revision")?;

        let boundary = unique_boundary_checkpoint(
            txn,
            &self.parent_session_id,
            self.stable_subagent_id.as_deref(),
        )
        .await?;
        let link_state = if boundary.is_some() {
            "resolved"
        } else {
            "unresolved"
        };
        let link_timestamp_ms = Utc::now().timestamp_millis();
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
            [
                self.checkpoint_id.clone().into(),
                self.parent_session_id.clone().into(),
                link_state.into(),
                boundary.into(),
                self.stable_subagent_id.clone().into(),
                link_timestamp_ms.into(),
                link_timestamp_ms.into(),
            ],
        ))
        .await
        .context("insert subagent content boundary association")?;

        let advanced = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_subagent_content_claim
                 SET revision_cursor = ?, sync_revision = sync_revision + 1,
                     current_revision = ?, current_checkpoint_id = ?, current_digest = ?,
                     state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                     owner = NULL, lease_expires_at = NULL, updated_at = ?
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'reserved'
                   AND attempt_digest = ? AND attempt_checkpoint_id = ?
                   AND owner = ? AND fence_token = ? AND revision_cursor = ?",
                [
                    self.revision.into(),
                    self.revision.into(),
                    self.checkpoint_id.clone().into(),
                    self.content_digest.clone().into(),
                    Utc::now().timestamp_millis().into(),
                    self.parent_session_id.clone().into(),
                    self.provider_kind.clone().into(),
                    self.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    self.content_digest.clone().into(),
                    self.checkpoint_id.clone().into(),
                    self.owner.clone().into(),
                    self.fence_token.into(),
                    revision_cursor.into(),
                ],
            ))
            .await
            .context("advance current subagent content leaf")?;
        if advanced.rows_affected() != 1 {
            bail!("subagent content current leaf lost its fenced final update");
        }
        Ok(())
    }
}

async fn current_parent_commit(conn: &DatabaseConnection) -> Result<Option<String>> {
    match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
        Ok(commit) => Ok(commit.map(|hash| hash.to_string())),
        Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
            if detail.contains("HEAD reference is missing") =>
        {
            Ok(None)
        }
        Err(error) => Err(anyhow!(
            "failed to resolve HEAD for subagent content checkpoint: {error}"
        )),
    }
}

fn safe_content_projection(
    source: &DiscoveredSubagentContent,
) -> Result<(RedactedBytes, serde_json::Value, bool, usize)> {
    let mut turns = normalize_claude_transcript(&source.bytes);
    let report = redact_turns_with_report(&mut turns);
    let partial = source.partial
        || source.malformed_lines > 0
        || turns.is_empty()
        || turns
            .iter()
            .any(|turn| turn.completeness.as_db_str() == "incomplete");
    let mut bytes = Vec::new();
    for turn in &turns {
        let projection = safe_turn_projection(&source.provider_kind, turn);
        serde_json::to_writer(&mut bytes, &projection)
            .context("serialize safe subagent turn projection")?;
        bytes.push(b'\n');
    }
    Ok((
        RedactedBytes::new_unchecked(bytes),
        serde_json::to_value(report).context("serialize subagent redaction report")?,
        partial,
        turns.len(),
    ))
}

fn subagent_content_identity_digest(
    transcript: &RedactedBytes,
    partial: bool,
    malformed_lines: usize,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-content-v1\0");
    digest.update((transcript.as_ref().len() as u64).to_be_bytes());
    digest.update(transcript.as_ref());
    digest.update([u8::from(partial)]);
    digest.update((malformed_lines as u64).to_be_bytes());
    hex::encode(digest.finalize())
}

struct PreparedSubagentContent<'a> {
    source: &'a DiscoveredSubagentContent,
    transcript: RedactedBytes,
    report_value: serde_json::Value,
    partial: bool,
    turn_count: usize,
    content_digest: String,
}

async fn traces_head<C: ConnectionTrait>(conn: &C) -> Result<Option<String>> {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT `commit` FROM reference
         WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
        [crate::internal::branch::TRACES_BRANCH.into()],
    ))
    .await
    .context("read refs/libra/traces for subagent durability proof")?
    .map(|row| row.try_get_by::<Option<String>, _>("commit"))
    .transpose()
    .context("decode refs/libra/traces for subagent durability proof")
    .map(Option::flatten)
}

/// Build one durability proof for every currently unchanged child source.
/// Callers process these sources before any changed source can append a new
/// traces commit, so each per-source reservation only needs to recheck the
/// exact catalog identity and one ref value rather than walking global history.
async fn build_unchanged_durability_proof(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    prepared: &[PreparedSubagentContent<'_>],
    deadline: Option<Instant>,
) -> Result<UnchangedDurabilityProof> {
    let mut candidates = Vec::<(String, DurableCheckpointIdentity)>::new();
    let mut source_checkpoints = HashMap::new();
    for item in prepared {
        let source = item.source;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT c.current_checkpoint_id, cp.traces_commit, cp.tree_oid,
                        cp.metadata_blob_oid
                 FROM agent_subagent_content_claim c
                 JOIN agent_subagent_content_revision r
                   ON r.parent_session_id = c.parent_session_id
                  AND r.provider_kind = c.provider_kind
                  AND r.source_key = c.source_key
                  AND r.content_schema_version = c.content_schema_version
                  AND r.revision = c.current_revision
                  AND r.checkpoint_id = c.current_checkpoint_id
                  AND r.content_digest = c.current_digest
                 JOIN agent_checkpoint cp
                   ON cp.checkpoint_id = c.current_checkpoint_id
                  AND cp.session_id = c.parent_session_id
                  AND cp.scope = 'subagent'
                 JOIN agent_subagent_link l
                   ON l.content_checkpoint_id = c.current_checkpoint_id
                  AND l.parent_session_id = c.parent_session_id
                 WHERE c.parent_session_id = ? AND c.provider_kind = ?
                   AND c.source_key = ? AND c.content_schema_version = ?
                   AND c.current_revision > 0 AND c.current_digest = ?",
                [
                    parent_session_id.into(),
                    source.provider_kind.clone().into(),
                    source.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    item.content_digest.clone().into(),
                ],
            ))
            .await
            .context("load unchanged subagent content for batch durability proof")?;
        let Some(row) = row else {
            continue;
        };
        let checkpoint_id: String = row.try_get_by("current_checkpoint_id")?;
        let identity = DurableCheckpointIdentity {
            traces_commit: row.try_get_by("traces_commit")?,
            tree_oid: row.try_get_by("tree_oid")?,
            metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
        };
        source_checkpoints.insert(
            (
                source.provider_kind.clone(),
                source.source_key.clone(),
                item.content_digest.clone(),
            ),
            checkpoint_id.clone(),
        );
        if !candidates
            .iter()
            .any(|(existing, _)| existing == &checkpoint_id)
        {
            candidates.push((checkpoint_id, identity));
        }
    }
    if candidates.is_empty() {
        return Ok(UnchangedDurabilityProof::default());
    }
    let head_before = traces_head(conn)
        .await?
        .context(
            "refs/libra/traces is missing while proving unchanged subagent content; run `libra agent doctor` before replaying these sources",
        )?;
    let specs = candidates
        .iter()
        .map(
            |(checkpoint_id, identity)| history::CheckpointDurabilitySpec {
                checkpoint_id,
                traces_commit: &identity.traces_commit,
                tree_oid: &identity.tree_oid,
                metadata_blob_oid: &identity.metadata_blob_oid,
            },
        )
        .collect::<Vec<_>>();
    history::checkpoint_snapshot_durable_oids(conn, storage_root, &specs, deadline)
        .await
        .context(
            "subagent content checkpoint objects or traces reachability are incomplete; run `libra agent doctor` before replaying these sources",
        )?;
    let head_after = traces_head(conn).await?;
    if head_after.as_deref() != Some(head_before.as_str()) {
        bail!("refs/libra/traces changed during the batch durability proof; retry the capture");
    }
    Ok(UnchangedDurabilityProof {
        traces_head: Some(head_before),
        checkpoints: candidates.into_iter().collect(),
        source_checkpoints,
    })
}

/// Persist every discovered source independently.  A byte-identical current
/// digest is a no-op; changed content advances only that source's revision.
pub(crate) async fn capture_discovered_subagent_contents(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<Instant>,
) -> Result<SubagentCaptureSummary> {
    let mut summary = SubagentCaptureSummary {
        discovered: sources.len(),
        ..SubagentCaptureSummary::default()
    };
    if let Err(source) = capture_discovered_subagent_contents_inner(
        conn,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
        &mut summary,
    )
    .await
    {
        return Err(SubagentCaptureProgressError { summary, source }.into());
    }
    Ok(summary)
}

async fn capture_discovered_subagent_contents_inner(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<Instant>,
    summary: &mut SubagentCaptureSummary,
) -> Result<()> {
    if !matches!(source_channel, "live" | "import") {
        bail!("invalid subagent content source channel");
    }
    let parent = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT agent_kind,
                    json_extract(metadata_json, '$.capture_incarnation') AS capture_incarnation
             FROM agent_session WHERE session_id = ?",
            [parent_session_id.into()],
        ))
        .await
        .context("verify subagent content parent session")?
        .context("subagent content parent session does not exist")?;
    let parent_agent_kind: String = parent.try_get_by("agent_kind")?;
    let capture_incarnation: Option<String> = parent.try_get_by("capture_incarnation")?;
    let mut incarnation_sources = sources.to_vec();
    for source in &mut incarnation_sources {
        source.source_key =
            source_key_for_incarnation(&source.source_key, capture_incarnation.as_deref())?;
    }
    // Programmatic callers predating M5 did not supply a deadline. Bound
    // contention waits anyway so an abandoned live lease can never turn this
    // API into an unbounded hang.
    let effective_deadline =
        deadline.or_else(|| Instant::now().checked_add(Duration::from_secs(5)));
    summary.discovered = incarnation_sources.len();
    let mut prepared = Vec::with_capacity(incarnation_sources.len());
    for source in &incarnation_sources {
        ensure_before_deadline(effective_deadline)?;
        validate_source_identity(&source.provider_kind, &source.source_key)?;
        if source.provider_kind != parent_agent_kind {
            bail!(
                "subagent content provider does not match its parent capture session (fail-closed)"
            );
        }
        let (transcript, report_value, partial, turn_count) = safe_content_projection(source)?;
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        prepared.push(PreparedSubagentContent {
            source,
            transcript,
            report_value,
            partial,
            turn_count,
            content_digest,
        });
    }
    let mut durability_proof = build_unchanged_durability_proof(
        conn,
        storage_root,
        parent_session_id,
        &prepared,
        effective_deadline,
    )
    .await?;
    // Proven unchanged leaves must be checked before this invocation appends
    // any changed/new child commit, otherwise its own writes would invalidate
    // the exact-head proof and force another global traversal.
    prepared.sort_by_key(|item| {
        durability_proof
            .source_checkpoint(item.source, &item.content_digest)
            .is_none()
    });
    for index in 0..prepared.len() {
        let item = &prepared[index];
        let source = item.source;
        let transcript = &item.transcript;
        let report_value = &item.report_value;
        let partial = item.partial;
        let turn_count = item.turn_count;
        let content_digest = &item.content_digest;
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let owner = format!("subagent:{}:{}", std::process::id(), uuid::Uuid::new_v4());
        let now_ms = Utc::now().timestamp_millis();
        let mut durability_proof_refreshes = 0_usize;
        let reservation = loop {
            ensure_before_deadline(effective_deadline).context(
                "another writer still owns the subagent content source; retry the capture",
            )?;
            let outcome = reserve_source(
                conn,
                parent_session_id,
                source,
                ReservationAttempt {
                    content_digest,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                },
                &durability_proof,
            )
            .await?;
            match outcome {
                ReservationOutcome::Inflight { lease_expires_at } => {
                    let now = Utc::now().timestamp_millis();
                    let until_lease_ms = lease_expires_at.saturating_sub(now).max(1);
                    let wait_ms = u64::try_from(until_lease_ms).unwrap_or(25).min(25);
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                }
                ReservationOutcome::DurabilityProofStale => {
                    durability_proof_refreshes = durability_proof_refreshes.saturating_add(1);
                    if durability_proof_refreshes > 2 {
                        bail!(
                            "subagent content changed repeatedly while proving durability; retry the capture"
                        );
                    }
                    // A concurrent writer may have published one or more
                    // identical children after the initial batch snapshot.
                    // Rebuild one proof for this child and every remaining
                    // child rather than falling back to per-child traversals.
                    durability_proof = build_unchanged_durability_proof(
                        conn,
                        storage_root,
                        parent_session_id,
                        &prepared[index..],
                        effective_deadline,
                    )
                    .await?;
                }
                settled => break settled,
            }
        };
        let (fence_token, revision) = match reservation {
            ReservationOutcome::Reserved {
                fence_token,
                revision,
            } => (fence_token, revision),
            ReservationOutcome::Unchanged { checkpoint_exists } => {
                if checkpoint_exists {
                    refresh_current_link(conn, parent_session_id, source).await?;
                }
                summary.skipped_unchanged += 1;
                continue;
            }
            ReservationOutcome::Inflight { .. } => {
                bail!("subagent content reservation loop returned an unsettled outcome")
            }
            ReservationOutcome::DurabilityProofStale => {
                bail!("subagent content reservation loop returned a stale durability proof")
            }
        };

        if let Err(error) = subagent_content_test_failpoint("after_reservation") {
            let cleanup = release_reservation(
                conn,
                parent_session_id,
                source,
                &checkpoint_id,
                &owner,
                fence_token,
            )
            .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    preserve_cleanup_error(error, "release subagent content reservation", cleanup)
                }
            });
        }

        let marker = TracesInflightMarker::new(parent_session_id, &checkpoint_id, now_ms);
        let marker_generation = marker
            .generation
            .as_deref()
            .context("new subagent content marker has no writer generation")?;
        if let Err(error) = history::register_traces_write_attempt(conn, &marker, &[]).await {
            let error = error.context("register subagent content traces write attempt");
            let cleanup = release_reservation(
                conn,
                parent_session_id,
                source,
                &checkpoint_id,
                &owner,
                fence_token,
            )
            .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    preserve_cleanup_error(error, "release subagent content reservation", cleanup)
                }
            });
        }

        let written: Result<_> = async {
            subagent_content_test_failpoint("after_marker")?;
            let created_at = Utc::now().timestamp();
            let parent_commit = current_parent_commit(conn).await?;
            let metadata_value = serde_json::json!({
                "schema_version": history::CHECKPOINT_METADATA_SCHEMA_VERSION,
                "checkpoint_id": checkpoint_id,
                "session_id": parent_session_id,
                "agent_kind": source.provider_kind,
                "scope": "subagent",
                "parent_checkpoint_id": null,
                "subagent_session_id": source.stable_subagent_id,
                "tool_use_id": null,
                "description": "subagent content",
                "created_at": created_at,
                "subagent": {
                    "provenance": "content",
                    "source_key": source.source_key,
                    "content_schema_version": SUBAGENT_CONTENT_SCHEMA_VERSION,
                    "revision": revision,
                    "source_channel": source_channel,
                    "partial": partial,
                    "malformed_lines": source.malformed_lines,
                    "turn_count": turn_count,
                    "link_state": if source.stable_subagent_id.is_some() {
                        "pending_unique_match"
                    } else {
                        "unresolved"
                    },
                },
            });
            let metadata_bytes = serde_json::to_vec_pretty(&metadata_value)
                .context("serialize subagent content metadata")?;
            let (metadata, _) = Redactor::new_default().redact(&metadata_bytes);
            let report_bytes = serde_json::to_vec_pretty(&report_value)
                .context("serialize subagent content redaction report")?;
            let (report, _) = Redactor::new_default().redact(&report_bytes);
            let empty_events = RedactedBytes::new_unchecked(Vec::new());

            let objects_dir = storage_root.join("objects");
            std::fs::create_dir_all(&objects_dir)
                .context("create objects directory for subagent content")?;
            let manager = HistoryManager::new_with_ref(
                Arc::new(ClientStorage::init(objects_dir)),
                storage_root.to_path_buf(),
                Arc::new(conn.clone()),
                crate::internal::branch::TRACES_BRANCH,
            );
            let plan = ContentCommitPlan {
                parent_session_id: parent_session_id.to_string(),
                provider_kind: source.provider_kind.clone(),
                source_key: source.source_key.clone(),
                content_digest: content_digest.clone(),
                checkpoint_id: checkpoint_id.clone(),
                owner: owner.clone(),
                fence_token,
                revision,
                parent_commit: parent_commit.clone(),
                source_channel: source_channel.to_string(),
                partial,
                stable_subagent_id: source.stable_subagent_id.clone(),
                created_at,
            };
            manager
                .append_checkpoint_commit(CheckpointCommitParams {
                    checkpoint_id: &checkpoint_id,
                    session_id: parent_session_id,
                    marker_generation,
                    agent_kind: &source.provider_kind,
                    parent_commit: parent_commit.as_deref(),
                    scope: CheckpointScope::Subagent,
                    tool_use_id: None,
                    metadata_json: &metadata,
                    transcript_redacted: transcript,
                    lifecycle_events_jsonl: &empty_events,
                    redaction_report_json: &report,
                    txn_extra: Some(&plan),
                    deadline,
                })
                .await
                .context("append subagent content checkpoint")
        }
        .await;
        let written = match written {
            Ok(written) => written,
            Err(error) => {
                let mut error = error;
                if let Err(cleanup) = release_reservation(
                    conn,
                    parent_session_id,
                    source,
                    &checkpoint_id,
                    &owner,
                    fence_token,
                )
                .await
                {
                    error = preserve_cleanup_error(
                        error,
                        "release subagent content reservation",
                        cleanup,
                    );
                }
                if let Err(cleanup) = history::clear_non_cleanup_traces_inflight_marker(
                    conn,
                    parent_session_id,
                    &checkpoint_id,
                    marker_generation,
                )
                .await
                {
                    error = preserve_cleanup_error(
                        error,
                        "clear failed subagent content in-flight marker",
                        cleanup,
                    );
                }
                return Err(error);
            }
        };
        if let Err(error) = history::clear_non_cleanup_traces_inflight_marker(
            conn,
            parent_session_id,
            &checkpoint_id,
            &written.marker_generation,
        )
        .await
        {
            tracing::warn!(
                checkpoint_id = %checkpoint_id,
                error = %format!("{error:#}"),
                "failed to clear committed subagent content in-flight marker"
            );
        }
        summary.checkpoints_written += 1;
        if partial {
            summary.partial_sources += 1;
            tracing::warn!(
                checkpoint_id = %checkpoint_id,
                malformed_lines = source.malformed_lines,
                "subagent content captured partially; malformed lines were skipped"
            );
        }
        subagent_content_test_failpoint("after_first_commit")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sea_orm::{Database, DatabaseConnection};
    use serial_test::serial;

    use super::*;
    use crate::internal::{
        ai::observed_agents::{ObservedAgent, SubagentAwareExtractor},
        db::migration::run_builtin_migrations,
    };

    #[test]
    fn embedded_host_without_libra_binary_disables_unbounded_discovery() {
        let directory = tempfile::tempdir().expect("tempdir");
        let host = directory
            .path()
            .join("target")
            .join("debug")
            .join("deps")
            .join("embedding-host");
        assert_eq!(helper_program_from(&host), None);
        let discovery = helper_unavailable_discovery();
        assert!(discovery.sources.is_empty());
        assert_eq!(discovery.bytes_read, 0);
        assert!(
            discovery
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("killable"))
        );
    }

    #[test]
    fn empty_or_metadata_only_child_content_is_partial() {
        for bytes in [b"".as_slice(), br#"{"type":"system","message":"metadata"}"#] {
            let (malformed, partial) =
                subagent_source_completeness(bytes, None).expect("classify child content");
            assert_eq!(malformed, 0);
            assert!(
                partial,
                "child content without a normalized turn is incomplete"
            );
        }
    }

    #[test]
    fn parent_side_child_validation_observes_the_absolute_deadline() {
        let error =
            subagent_source_completeness(&child_transcript("late child"), Some(Instant::now()))
                .expect_err("expired validation deadline must fail");
        assert!(error.to_string().contains("deadline"));
    }

    async fn test_store() -> (tempfile::TempDir, DatabaseConnection, PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let storage_root = directory.path().join(".libra");
        std::fs::create_dir_all(storage_root.join("objects")).expect("objects dir");
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("memory database");
        run_builtin_migrations(&conn).await.expect("migrations");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
        ))
        .await
        .expect("minimal ai_thread FK target");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version
             ) VALUES ('parent-session', 'claude_code', 'provider-session',
                       'active', '/repo', '{}', '{}', 1, 1, 1)"
                .to_string(),
        ))
        .await
        .expect("parent session");
        (directory, conn, storage_root)
    }

    fn child_transcript(text: &str) -> Vec<u8> {
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "user",
                "uuid": "child-user",
                "message": {"role": "user", "content": text}
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "child-assistant",
                "message": {
                    "role": "assistant",
                    "content": "done",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }
            })
        )
        .into_bytes()
    }

    async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("scalar query")
        .expect("scalar row")
        .try_get_by("n")
        .expect("scalar value")
    }

    async fn seed_boundary(
        conn: &DatabaseConnection,
        checkpoint_id: &str,
        stable_id: Option<&str>,
    ) {
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, 'parent-session', NULL, 'subagent', NULL,
                       ?, ?, ?, NULL, ?, 'subagent boundary', 1)",
            [
                checkpoint_id.into(),
                format!("tree-{checkpoint_id}").into(),
                format!("metadata-{checkpoint_id}").into(),
                format!("traces-{checkpoint_id}").into(),
                stable_id.map(str::to_string).into(),
            ],
        ))
        .await
        .expect("boundary row");
    }

    #[test]
    fn fixture_source_identity_rejects_basename_escape() {
        let source =
            DiscoveredSubagentContent::fixture("claude_code", "../child.jsonl", b"{}\n", None);
        assert!(validate_source_identity(&source.provider_kind, &source.source_key).is_err());
    }

    #[test]
    fn restored_capture_incarnation_namespaces_source_identity() {
        let source = format!("source/sha256/{}", "a".repeat(64));
        assert_eq!(
            source_key_for_incarnation(&source, None).expect("legacy source identity"),
            source
        );
        let first = source_key_for_incarnation(&source, Some(&"1".repeat(32)))
            .expect("first restored incarnation");
        let second = source_key_for_incarnation(&source, Some(&"2".repeat(32)))
            .expect("second restored incarnation");
        assert_ne!(first, source);
        assert_ne!(first, second);
        assert!(first.starts_with("source/sha256/"));
        assert_eq!(first.len(), "source/sha256/".len() + 64);
    }

    #[test]
    fn claude_subagent_attribution() {
        let parent = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "name": "Task", "input": {"prompt": "child"}},
                        {"type": "tool_use", "name": "Write", "input": {"file_path": "src/parent.rs"}}
                    ],
                    "usage": {"input_tokens": 10, "output_tokens": 4}
                }
            })
        );
        let child = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "name": "Edit", "input": {"file_path": "src/child.rs"}}
                    ],
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }
            })
        );
        let adapter = ClaudeCodeObservedAgent::new();
        assert!(adapter.as_subagent_aware_extractor().is_some());
        let aggregate = adapter
            .extract_parent_and_subagents(parent.as_bytes(), &[child.as_bytes()])
            .expect("multi-source extraction");
        let paths = aggregate
            .modified_files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["src/parent.rs", "src/child.rs"]);
        assert_eq!(aggregate.aggregate_usage.input_tokens, 13);
        assert_eq!(
            aggregate
                .subagent_usage
                .expect("child usage attributed")
                .input_tokens,
            3
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn claude_subagent_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "abcdef00-0000-0000-0000-000000000001";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        let target = home.path().join("target.jsonl");
        std::fs::write(&target, child_transcript("child")).expect("target");
        symlink(&target, directory.join("child.jsonl")).expect("symlink");
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("symlink must fail closed");
        assert!(format!("{error:#}").contains("symlink"));
        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn claude_subagent_discovery_enforces_budget_and_persists_only_opaque_identity() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo/customer-secret");
        let session_id = "abcdef00-0000-0000-0000-000000000002";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        let bytes = child_transcript("child");
        std::fs::write(directory.join("alice-secret.jsonl"), &bytes).expect("child transcript");

        let too_small = u64::try_from(bytes.len())
            .expect("fixture size")
            .saturating_sub(1);
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            too_small,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("configured byte budget must be enforced");
        assert!(format!("{error:#}").contains("input budget"));

        let discovery = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            u64::try_from(bytes.len()).expect("fixture size"),
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect("bounded discovery");
        assert_eq!(discovery.bytes_read, bytes.len() as u64);
        assert_eq!(discovery.sources.len(), 1);
        let source_key = &discovery.sources[0].source_key;
        assert!(source_key.starts_with("source/sha256/"));
        assert_eq!(source_key.len(), "source/sha256/".len() + 64);
        assert!(!source_key.contains("customer-secret"));
        assert!(!source_key.contains("alice-secret"));
        assert!(!source_key.contains(session_id));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn claude_subagent_discovery_supports_legacy_safe_session_components() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "Legacy.session_01";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        std::fs::write(directory.join("child.jsonl"), child_transcript("child"))
            .expect("child transcript");

        let discovery = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect("legacy safe session id must retain child discovery");
        assert_eq!(discovery.sources.len(), 1);
        assert!(claude_session_id_is_safe_path_component("legacy_01"));
        assert!(claude_session_id_is_safe_path_component("legacy.session"));
        assert!(!claude_session_id_is_safe_path_component(".."));
        assert!(!claude_session_id_is_safe_path_component("legacy/session"));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn claude_subagent_discovery_counts_non_json_entries_toward_directory_bound() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "abcdef00-0000-0000-0000-000000000003";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        for index in 0..=MAX_SUBAGENT_DIRECTORY_ENTRIES {
            std::fs::write(directory.join(format!("noise-{index}.txt")), b"").expect("noise entry");
        }
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("non-JSON entries must count toward enumeration limit");
        assert!(format!("{error:#}").contains("entry safety limit"));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[tokio::test]
    async fn subagent_content_repeat_single_visible_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        let first = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("first capture");
        let second = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("repeat capture");
        assert_eq!(first.checkpoints_written, 1);
        assert_eq!(second.skipped_unchanged, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE current_revision = 1"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn unchanged_children_share_one_global_durability_traversal() {
        let (_directory, conn, storage_root) = test_store().await;
        let sources = vec![
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/one.jsonl",
                &child_transcript("one"),
                None,
            ),
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/two.jsonl",
                &child_transcript("two"),
                None,
            ),
        ];
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &sources,
            "live",
            None,
        )
        .await
        .expect("initial child capture");

        let (replay, traversals) =
            history::count_checkpoint_snapshot_verifications(capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &sources,
                "live",
                None,
            ))
            .await;
        let replay = replay.expect("unchanged child replay");
        assert_eq!(replay.skipped_unchanged, 2);
        assert_eq!(traversals, 1, "all unchanged children use one traces walk");
    }

    #[tokio::test]
    async fn subagent_content_identity_includes_partial_and_malformed_state() {
        let (_directory, conn, storage_root) = test_store().await;
        let clean = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/state.jsonl",
            &child_transcript("same projection"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&clean),
            "live",
            None,
        )
        .await
        .expect("capture clean source");

        let mut malformed = clean.clone();
        malformed.malformed_lines = 1;
        let changed = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[malformed],
            "live",
            None,
        )
        .await
        .expect("capture newly malformed source");
        assert_eq!(changed.checkpoints_written, 1);

        let restored = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[clean],
            "live",
            None,
        )
        .await
        .expect("capture source restored to clean");
        assert_eq!(restored.checkpoints_written, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            3
        );
    }

    #[tokio::test]
    async fn unchanged_replay_clears_expired_matching_reservation() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/expired.jsonl",
            &child_transcript("same"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("initial capture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim
                 SET state = 'reserved', attempt_digest = current_digest,
                     attempt_checkpoint_id = 'expired-attempt', owner = 'crashed',
                     lease_expires_at = {}, fence_token = 7",
                Utc::now().timestamp_millis() - 1
            ),
        ))
        .await
        .expect("seed expired matching reservation");
        let replay = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("unchanged replay");
        assert_eq!(replay.skipped_unchanged, 1);
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, attempt_digest, owner, fence_token
                 FROM agent_subagent_content_claim"
                    .to_string(),
            ))
            .await
            .expect("query claim")
            .expect("claim row");
        assert_eq!(row.try_get_by::<String, _>("state").expect("state"), "idle");
        assert_eq!(
            row.try_get_by::<Option<String>, _>("attempt_digest")
                .expect("attempt digest"),
            None
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner").expect("owner"),
            None
        );
        assert_eq!(row.try_get_by::<i64, _>("fence_token").expect("fence"), 8);
    }

    #[tokio::test]
    async fn unchanged_replay_verifies_ref_and_all_catalog_objects() {
        for damaged_field in [
            "metadata_blob_oid",
            "tree_oid",
            "traces_commit",
            "traces_ref",
            "descendant_object",
        ] {
            let (_directory, conn, storage_root) = test_store().await;
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/durable.jsonl",
                &child_transcript("durable"),
                None,
            );
            capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("initial capture");
            if damaged_field == "traces_ref" {
                conn.execute_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "UPDATE reference SET `commit` = NULL
                     WHERE name = 'traces' AND kind = 'Branch'"
                        .to_string(),
                ))
                .await
                .expect("clear traces ref");
            } else if damaged_field == "descendant_object" {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        "SELECT checkpoint_id, traces_commit, tree_oid, metadata_blob_oid
                         FROM agent_checkpoint"
                            .to_string(),
                    ))
                    .await
                    .expect("query checkpoint identity")
                    .expect("checkpoint row");
                let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint");
                let traces_commit: String = row.try_get_by("traces_commit").expect("commit");
                let tree_oid: String = row.try_get_by("tree_oid").expect("tree");
                let metadata_blob_oid: String =
                    row.try_get_by("metadata_blob_oid").expect("metadata");
                let oid = history::checkpoint_leaf_durable_oids(
                    &conn,
                    &storage_root,
                    &checkpoint_id,
                    &traces_commit,
                    &tree_oid,
                    &metadata_blob_oid,
                )
                .await
                .expect("collect durable objects")
                .into_iter()
                .find(|oid| oid != &traces_commit && oid != &tree_oid && oid != &metadata_blob_oid)
                .expect("checkpoint has a descendant object");
                std::fs::remove_file(storage_root.join("objects").join(&oid[..2]).join(&oid[2..]))
                    .expect("remove descendant object");
            } else {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        format!("SELECT {damaged_field} AS oid FROM agent_checkpoint"),
                    ))
                    .await
                    .expect("query object oid")
                    .expect("checkpoint row");
                let oid: String = row.try_get_by("oid").expect("object oid");
                std::fs::remove_file(storage_root.join("objects").join(&oid[..2]).join(&oid[2..]))
                    .expect("remove checkpoint object");
            }
            let error = capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect_err("damaged durable identity must not replay as unchanged");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("libra agent doctor"),
                "{damaged_field}: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn supplied_catalog_validation_rejects_an_existing_corrupt_object() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/cloud-restore.jsonl",
            &child_transcript("cloud restore"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("capture cloud-restore durability fixture");
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id, traces_commit, tree_oid, metadata_blob_oid
                 FROM agent_checkpoint"
                    .to_string(),
            ))
            .await
            .expect("query checkpoint identity")
            .expect("checkpoint row");
        let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint");
        let traces_commit: String = row.try_get_by("traces_commit").expect("commit");
        let tree_oid: String = row.try_get_by("tree_oid").expect("tree");
        let metadata_blob_oid: String = row.try_get_by("metadata_blob_oid").expect("metadata");
        let spec = history::CheckpointDurabilitySpec {
            checkpoint_id: &checkpoint_id,
            traces_commit: &traces_commit,
            tree_oid: &tree_oid,
            metadata_blob_oid: &metadata_blob_oid,
        };
        let durable =
            history::checkpoint_rows_snapshot_durable_oids(&conn, &storage_root, &[spec], None)
                .await
                .expect("validate supplied checkpoint catalog");
        let damaged = durable
            .into_iter()
            .find(|oid| oid != &traces_commit && oid != &tree_oid && oid != &metadata_blob_oid)
            .expect("checkpoint has a descendant object");
        std::fs::write(
            storage_root
                .join("objects")
                .join(&damaged[..2])
                .join(&damaged[2..]),
            b"corrupt-existing-object",
        )
        .expect("corrupt an existing loose object");

        let error =
            history::checkpoint_rows_snapshot_durable_oids(&conn, &storage_root, &[spec], None)
                .await
                .expect_err("path existence must not satisfy restore durability");
        assert!(
            format!("{error:#}").contains("checkpoint"),
            "unexpected validation error: {error:#}"
        );
    }

    #[tokio::test]
    async fn subagent_content_replay_rejects_dangling_current_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("first capture");
        let checkpoint_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
            [checkpoint_id.into()],
        ))
        .await
        .expect("delete content catalog row");

        let error = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect_err("replay must not accept a dangling current leaf as unchanged");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("current leaf is incomplete"));
        assert!(rendered.contains("libra agent doctor"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn subagent_content_concurrent_same_source_single_append() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent.jsonl",
            &child_transcript("concurrent"),
            None,
        );
        let first_conn = conn.clone();
        let second_conn = conn.clone();
        let first_root = storage_root.clone();
        let second_root = storage_root.clone();
        let first_source = source.clone();
        let second_source = source.clone();
        let (first, second) = tokio::join!(
            async move {
                capture_discovered_subagent_contents(
                    &first_conn,
                    &first_root,
                    "parent-session",
                    &[first_source],
                    "live",
                    None,
                )
                .await
            },
            async move {
                capture_discovered_subagent_contents(
                    &second_conn,
                    &second_root,
                    "parent-session",
                    &[second_source],
                    "live",
                    None,
                )
                .await
            }
        );
        let first = first.expect("first concurrent capture");
        let second = second.expect("second concurrent capture");
        assert_eq!(first.checkpoints_written + second.checkpoints_written, 1);
        assert_eq!(
            first.skipped_unchanged
                + first.skipped_inflight
                + second.skipped_unchanged
                + second.skipped_inflight,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'subagent'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_concurrent_changed_digest_eventually_appends_both() {
        let (_directory, conn, storage_root) = test_store().await;
        let first_source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent-changed.jsonl",
            &child_transcript("first"),
            None,
        );
        let second_source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent-changed.jsonl",
            &child_transcript("second"),
            None,
        );
        let first_conn = conn.clone();
        let second_conn = conn.clone();
        let first_root = storage_root.clone();
        let second_root = storage_root.clone();
        let (first, second) = tokio::join!(
            async move {
                capture_discovered_subagent_contents(
                    &first_conn,
                    &first_root,
                    "parent-session",
                    &[first_source],
                    "live",
                    None,
                )
                .await
            },
            async move {
                capture_discovered_subagent_contents(
                    &second_conn,
                    &second_root,
                    "parent-session",
                    &[second_source],
                    "live",
                    None,
                )
                .await
            }
        );
        assert!(first.is_ok(), "first writer: {first:?}");
        assert!(second.is_ok(), "second writer: {second:?}");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE revision_cursor = 2 AND current_revision = 2 AND state = 'idle'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_waits_for_crashed_writer_lease_then_captures() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/crashed.jsonl",
            &child_transcript("final"),
            None,
        );
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        let stale = reserve_source(
            &conn,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "stale-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "crashed-writer",
            },
            &UnchangedDurabilityProof::default(),
        )
        .await
        .expect("seed crashed reservation");
        assert!(matches!(stale, ReservationOutcome::Reserved { .. }));
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim SET lease_expires_at = {}",
                Utc::now().timestamp_millis() + 40
            ),
        ))
        .await
        .expect("shorten crashed lease");
        let summary = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("capture after crashed lease expiry");
        assert_eq!(summary.checkpoints_written, 1);
        assert_eq!(summary.skipped_inflight, 0);
    }

    #[tokio::test]
    async fn subagent_content_live_reservation_deadline_returns_retryable_error_not_skip() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/busy.jsonl",
            &child_transcript("final"),
            None,
        );
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        reserve_source(
            &conn,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "different-inflight-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "live-writer",
            },
            &UnchangedDurabilityProof::default(),
        )
        .await
        .expect("seed live reservation");
        let deadline = Instant::now().checked_add(Duration::from_millis(30));
        let error = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            deadline,
        )
        .await
        .expect_err("busy source must not be reported as a successful skip");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("another writer"));
        assert!(rendered.contains("retry"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn subagent_content_lease_takeover_on_expiry() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/stale.jsonl",
            &child_transcript("stale-owner"),
            None,
        );
        let (projected, _, _, _) = safe_content_projection(&source).expect("safe projection");
        let digest = hex::encode(Sha256::digest(projected.as_ref()));
        let now_ms = Utc::now().timestamp_millis();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                current_revision, current_checkpoint_id, current_digest, state,
                attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, NULL, NULL, 'reserved', ?, ?, ?, ?, 1, ?, ?)",
            [
                "parent-session".into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                digest.into(),
                "stale-checkpoint".into(),
                "crashed-owner".into(),
                now_ms.saturating_sub(1).into(),
                now_ms.saturating_sub(SUBAGENT_CONTENT_LEASE_MS).into(),
                now_ms.saturating_sub(1).into(),
            ],
        ))
        .await
        .expect("seed expired reservation");

        let recovered = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "import",
            None,
        )
        .await
        .expect("take over expired reservation");
        assert_eq!(recovered.checkpoints_written, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE current_revision = 1 AND state = 'idle' AND fence_token = 2
                   AND owner IS NULL AND lease_expires_at IS NULL"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_changed_source_advances_source_revision_without_parent_link() {
        let (_directory, conn, storage_root) = test_store().await;
        let first = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        let second = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("second"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[first],
            "live",
            None,
        )
        .await
        .expect("first revision");
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[second],
            "live",
            None,
        )
        .await
        .expect("second revision");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE current_revision = 2"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE scope = 'subagent' AND parent_checkpoint_id IS NULL"
            )
            .await,
            2,
            "content revisions must not overload structural parent linkage"
        );
    }

    #[tokio::test]
    async fn subagent_content_prune_repoints_current_source_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        for text in ["first", "second"] {
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/child.jsonl",
                &child_transcript(text),
                None,
            );
            capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect("content revision");
        }
        let current_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(storage_root.join("objects"))),
            storage_root.clone(),
            Arc::new(conn.clone()),
            crate::internal::branch::TRACES_BRANCH,
        );
        let sync_revision_before = scalar(
            &conn,
            "SELECT sync_revision AS n FROM agent_subagent_content_claim",
        )
        .await;
        let pruned = manager
            .prune_checkpoint_commits(&[current_id])
            .await
            .expect("prune current content revision");
        assert_eq!(pruned.removed_checkpoints, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT sync_revision AS n FROM agent_subagent_content_claim"
            )
            .await,
            sync_revision_before + 1,
            "pruning a current leaf must advance its cloud sync generation"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE current_revision = 1 AND current_checkpoint_id IS NOT NULL"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        let third = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("third"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[third],
            "live",
            None,
        )
        .await
        .expect("append after pruning current revision");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                 WHERE revision IN (1, 3)"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE revision_cursor = 3 AND current_revision = 3"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_reservation_blocks_prune_before_marker() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("current"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("current content");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim
                 SET state = 'reserved', attempt_digest = 'next',
                     attempt_checkpoint_id = 'attempt-next', owner = 'writer',
                     lease_expires_at = {}, updated_at = {}",
                Utc::now().timestamp_millis() + 60_000,
                Utc::now().timestamp_millis()
            ),
        ))
        .await
        .expect("seed reservation-before-marker window");
        let current_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(storage_root.join("objects"))),
            storage_root,
            Arc::new(conn),
            crate::internal::branch::TRACES_BRANCH,
        );
        let error = manager
            .prune_checkpoint_commits(&[current_id])
            .await
            .expect_err("live source reservation must block whole-chain prune");
        assert!(matches!(
            error.downcast_ref::<history::SubagentContentReservationPruneGuard>(),
            Some(history::SubagentContentReservationPruneGuard { .. })
        ));
    }

    #[tokio::test]
    #[serial]
    async fn later_source_failure_preserves_committed_child_progress() {
        let (_directory, conn, storage_root) = test_store().await;
        let sources = [
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/first.jsonl",
                &child_transcript("first"),
                None,
            ),
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/second.jsonl",
                &child_transcript("second"),
                None,
            ),
        ];
        let error = TEST_SUBAGENT_CONTENT_FAILPOINT
            .scope(
                Some("after_first_commit"),
                capture_discovered_subagent_contents(
                    &conn,
                    &storage_root,
                    "parent-session",
                    &sources,
                    "import",
                    None,
                ),
            )
            .await
            .expect_err("failure after the first durable child must surface");
        let progress = error
            .downcast_ref::<SubagentCaptureProgressError>()
            .expect("typed partial child progress");
        assert_eq!(progress.summary().checkpoints_written, 1);
        assert_eq!(progress.summary().discovered, 2);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    #[serial]
    async fn subagent_content_failure_injection_leaves_no_visible_leaf_or_marker() {
        for stage in ["after_reservation", "after_marker", "before_final_sql"] {
            let (_directory, conn, storage_root) = test_store().await;
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/child.jsonl",
                &child_transcript(stage),
                None,
            );
            let error = TEST_SUBAGENT_CONTENT_FAILPOINT
                .scope(
                    Some(stage),
                    capture_discovered_subagent_contents(
                        &conn,
                        &storage_root,
                        "parent-session",
                        &[source],
                        "live",
                        None,
                    ),
                )
                .await
                .expect_err("injected failure must surface");
            assert!(format!("{error:#}").contains(stage));
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'subagent'"
                )
                .await,
                0,
                "{stage}: no catalog leaf may become visible"
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                )
                .await,
                0,
                "{stage}: no revision may commit"
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                     WHERE state = 'idle' AND current_revision = 0"
                )
                .await,
                1,
                "{stage}: reservation must be replayable"
            );
            let marker_count = scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'",
            )
            .await;
            if stage == "before_final_sql" {
                assert_eq!(
                    marker_count, 1,
                    "object-producing rejection must retain its durable cleanup job"
                );
                assert_eq!(
                    scalar(
                        &conn,
                        "SELECT COUNT(*) AS n FROM metadata_kv
                         WHERE scope = 'agent_traces_inflight'
                           AND json_extract(value, '$.cleanup_pending') = 1"
                    )
                    .await,
                    1
                );
            } else {
                assert_eq!(
                    marker_count, 0,
                    "{stage}: empty ordinary in-flight marker must be cleared"
                );
            }
        }
    }

    #[tokio::test]
    async fn subagent_content_surfaces_reservation_cleanup_failure() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("cleanup-failure"),
            None,
        );
        let error = TEST_SUBAGENT_CONTENT_FAILPOINT
            .scope(
                Some("after_reservation,before_release_reservation"),
                capture_discovered_subagent_contents(
                    &conn,
                    &storage_root,
                    "parent-session",
                    &[source],
                    "live",
                    None,
                ),
            )
            .await
            .expect_err("primary and cleanup failures must both surface");
        let message = format!("{error:#}");
        assert!(message.contains("after_reservation"));
        assert!(message.contains("release subagent content reservation"));
        assert!(message.contains("before_release_reservation"));
        assert!(message.contains("libra agent doctor"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE state = 'reserved'"
            )
            .await,
            1,
            "the surfaced cleanup failure must describe the still-live reservation"
        );
    }

    #[tokio::test]
    async fn subagent_boundary_without_stable_id_remains_unresolved() {
        let (_directory, conn, storage_root) = test_store().await;
        seed_boundary(&conn, "boundary-without-id", None).await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("unresolved"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("content capture");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_unique_id_links_boundary_without_history_rewrite() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("stable"),
            Some("stable-child-1"),
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("initial unresolved content");
        let original_traces: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT cp.traces_commit FROM agent_checkpoint cp
                 JOIN agent_subagent_content_claim c
                   ON c.current_checkpoint_id = cp.checkpoint_id"
                    .to_string(),
            ))
            .await
            .expect("traces query")
            .expect("content row")
            .try_get_by("traces_commit")
            .expect("traces commit");
        seed_boundary(&conn, "boundary-stable", Some("stable-child-1")).await;
        let repeated = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("link refresh");
        assert_eq!(repeated.skipped_unchanged, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1,
            "link refresh must not append content history"
        );
        let linked = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT l.link_state, l.boundary_checkpoint_id, cp.traces_commit
                 FROM agent_subagent_link l
                 JOIN agent_checkpoint cp ON cp.checkpoint_id = l.content_checkpoint_id"
                    .to_string(),
            ))
            .await
            .expect("link query")
            .expect("link row");
        assert_eq!(
            linked.try_get_by::<String, _>("link_state").expect("state"),
            "resolved"
        );
        assert_eq!(
            linked
                .try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                .expect("boundary"),
            Some("boundary-stable".to_string())
        );
        assert_eq!(
            linked
                .try_get_by::<String, _>("traces_commit")
                .expect("traces"),
            original_traces,
            "association must not rewrite immutable traces history"
        );
    }

    #[tokio::test]
    async fn deleting_boundary_preserves_content_as_unresolved() {
        let (_directory, conn, storage_root) = test_store().await;
        seed_boundary(&conn, "boundary-stable", Some("stable-child-1")).await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("stable"),
            Some("stable-child-1"),
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("resolved content capture");

        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
            ["boundary-stable".into()],
        ))
        .await
        .expect("delete boundary checkpoint");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL
                   AND stable_subagent_id = 'stable-child-1'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE updated_at >= 1000000000000"
            )
            .await,
            1,
            "link creation, refresh, and boundary-delete trigger timestamps use milliseconds"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE sync_revision = 2"
            )
            .await,
            1,
            "boundary deletion advances the explicit link sync generation"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1,
            "deleting association evidence must not delete content history"
        );
    }
}
