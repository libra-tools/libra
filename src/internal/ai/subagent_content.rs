//! Source-scoped subagent transcript capture (plan-20260713 M5 / DR-06).
//!
//! Provider files are discovered beneath a held, no-follow directory and are
//! converted to snapshot-owned projections before persistence. Claude keeps
//! its legacy allowlist; Codex retains credential-scanned ordinary thinking. The
//! checkpoint catalog row, source revision/current leaf, association row, and
//! traces ref CAS commit atomically through [`history::TracesTxnExtra`].

#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::{
    collections::HashMap,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(any(unix, windows))]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(unix, windows))]
use tokio::io::AsyncWriteExt;

#[cfg(any(unix, windows))]
use super::authorized_read::{
    CancellationSafeChild, StrictBoundedRead, configure_private_helper_process_group,
    read_async_strictly_bounded, read_strictly_bounded,
};
#[cfg(any(unix, windows))]
use super::observed_agents::{
    claude_project_slug, claude_session_dir, open_file_beneath_pinned_provider_directory,
    open_provider_directory_for_discovery,
};
use super::{
    capture::{
        catalog::{
            CaptureCatalogApplyRequest, CaptureCatalogError, CaptureCatalogFinalizerRecovery,
            ChildBoundaryReplayFence, ChildCheckpointRequirement, ChildPreparationFence,
            child_boundary_replay_fence, child_preparation_fence, child_preparation_for_recovery,
            child_recovery_is_pending,
        },
        snapshot::{CaptureSnapshot, CaptureSnapshotPolicy, CaptureSnapshotService},
    },
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    history::{
        self, CheckpointCommitParams, CheckpointScope, HistoryManager, TracesCommitCtx,
        TracesInflightMarker, TracesTxnExtra,
    },
    hooks::runtime::{
        CaptureSourceCommitmentDomain, derive_capture_source_commitment_in_scope_until,
    },
    observed_agents::{
        ClaudeCodeObservedAgent, ExportAuthorized, MAX_REDACTION_MATCH_SAMPLES, RedactedBytes,
        RedactionReport, Redactor, TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource,
        claude_session_id_is_safe_path_component, normalize_claude_transcript,
        normalize_claude_transcript_until, parse_canon_value, redact_turns_with_report,
        safe_turn_projection,
    },
};
use crate::utils::client_storage::ClientStorage;

/// New content claims use a repository-keyed HMAC source key.  Schema V1
/// rows remain immutable/read-only proof: a live capture must never create a
/// parallel V2 history for a source that still has V1 recovery state.
const SUBAGENT_CONTENT_SCHEMA_VERSION_V1: i64 = 1;
pub const SUBAGENT_CONTENT_SCHEMA_VERSION: i64 = 2;
pub const SUBAGENT_DISCOVERY_HELPER_ARG: &str = "--libra-internal-agent-subagent-discovery-helper";
pub const SUBAGENT_DISCOVERY_HELPER_INPUT_CAP: u64 = 1024 * 1024;
// A discovery response starts with a small JSON metadata header followed by
// direct raw source segments. Sending sources as JSON/base64 would retain the
// pipe buffer and a second owned base64 String in the deadline-owning parent,
// exceeding ACF-03's 2.5x raw working-set budget.
const SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES: usize = 4;
const SUBAGENT_DISCOVERY_HELPER_HEADER_CAP: u64 = 64 * 1024;
pub const SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP: u64 = TRANSCRIPT_READ_HARD_CAP_BYTES
    + SUBAGENT_DISCOVERY_HELPER_HEADER_CAP
    + SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES as u64;
/// Private, killable helper for normalize/redact/project work over one held
/// child transcript. The parent never performs that CPU-heavy work while it
/// is answerable to a hook/import deadline.
pub const SUBAGENT_PROJECTION_HELPER_ARG: &str =
    "--libra-internal-agent-subagent-projection-helper";
const SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES: usize = 1 + 8;
const CODEX_CHILD_BINDING_CAP: usize = 512;
const CODEX_CHILD_USAGE_CAP: usize = 512;
pub const SUBAGENT_PROJECTION_HELPER_INPUT_CAP: u64 = TRANSCRIPT_READ_HARD_CAP_BYTES
    + SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES as u64
    + 2
    + CODEX_CHILD_BINDING_CAP as u64
    + 8;
// A projection may need modest JSON escaping/allowlist framing expansion, but
// the parent retains the native child buffer while it drains helper output.
// Keep proportional output below 1.5x input so the two buffers stay within
// ACF-03's 2.5x working-set budget (apart from the small bounded report/frame).
// Short, valid JSONL transcripts also need a fixed schema floor: their typed
// projection has object-key overhead that is not proportional to a tiny raw
// payload. The floor is fixed and bounded, never an input-controlled expansion.
const SUBAGENT_PROJECTION_OUTPUT_NUMERATOR: u64 = 3;
const SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR: u64 = 2;
const SUBAGENT_PROJECTION_TRANSCRIPT_CAP: u64 = TRANSCRIPT_READ_HARD_CAP_BYTES
    * SUBAGENT_PROJECTION_OUTPUT_NUMERATOR
    / SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR;
const SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP: u64 = 64 * 1024;
const SUBAGENT_PROJECTION_REPORT_CAP: u64 = 64 * 1024;
const SUBAGENT_PROJECTION_FRAME_HEADER_BYTES: usize = 1 + 1 + 8 + 8 + 4 + 32;
pub const SUBAGENT_PROJECTION_HELPER_OUTPUT_CAP: u64 = SUBAGENT_PROJECTION_TRANSCRIPT_CAP
    + SUBAGENT_PROJECTION_REPORT_CAP
    + SUBAGENT_PROJECTION_FRAME_HEADER_BYTES as u64;
const SUBAGENT_PROJECTION_COMPLETE: u8 = 0;
const SUBAGENT_PROJECTION_FAILED: u8 = 1;
const SUBAGENT_PROJECTION_CODEX_COMPLETE: u8 = 2;
const SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE: u8 = 3;
const SUBAGENT_CONTENT_LEASE_MS: i64 = 60_000;
#[cfg_attr(windows, allow(dead_code))]
const MAX_SUBAGENT_DIRECTORY_ENTRIES: usize = 2_048;
pub(crate) const MAX_SUBAGENT_SOURCES_PER_CAPTURE: usize = 16;
const SUBAGENT_PARENT_PERSISTENCE_RESERVE: Duration = Duration::from_secs(20);
const SUBAGENT_RESERVATION_RELEASE_GRACE: Duration = Duration::from_millis(250);
const SUBAGENT_LIVE_WRITER_RETRY: &str =
    "another writer still owns the subagent content source; retry the capture";

#[derive(Debug, thiserror::Error)]
#[error("subagent discovery exhausted its parent-preservation deadline")]
pub(crate) struct SubagentDiscoveryDeadline;

#[derive(Debug, thiserror::Error)]
#[error("subagent content capture exceeded its command deadline")]
pub(crate) struct SubagentCaptureDeadline;

/// Transient identity established by the child-source I/O owner. Neither a
/// path nor a hook's reported agent id is itself a source authorization.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexChildBinding {
    root: String,
    parent: String,
    child: String,
    capture_session: String,
    hook_turn: Option<String>,
}

fn codex_child_identifier(value: &str, cap: usize) -> bool {
    !value.is_empty()
        && value.len() <= cap
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl CodexChildBinding {
    fn validate(&self) -> Result<()> {
        if [&self.root, &self.parent, &self.child]
            .into_iter()
            .any(|value| !codex_child_identifier(value, 64))
            || self.child == self.parent
            || !codex_child_identifier(&self.capture_session, 128)
            || self
                .hook_turn
                .as_deref()
                .is_some_and(|value| !codex_child_identifier(value, 64))
        {
            bail!("Codex child source has invalid bounded identity metadata");
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).context("encode Codex child identity metadata")?;
        if bytes.len() > CODEX_CHILD_BINDING_CAP {
            bail!("Codex child identity metadata exceeds its bounded frame");
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > CODEX_CHILD_BINDING_CAP {
            bail!("Codex child identity metadata has an invalid bounded frame");
        }
        let binding: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow!("Codex child identity metadata is invalid"))?;
        binding.validate()?;
        Ok(binding)
    }
}

/// One provider-authorized child transcript. Fields remain private so callers
/// cannot forge the discovery proof with arbitrary bytes.
#[derive(Clone)]
pub(crate) struct DiscoveredSubagentContent {
    provider_kind: String,
    /// A private helper-side/delivery proof only. Before persistence this is
    /// replaced with a repository-keyed V2 commitment. It must never reach a
    /// catalog claim, revision, checkpoint metadata, or trace object.
    source_key: String,
    /// Exact V1 lookup key retained only while the scoped writer decides
    /// whether a legacy immutable proof blocks V2 materialization.
    legacy_source_key: Option<String>,
    bytes: Vec<u8>,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
    codex_binding: Option<CodexChildBinding>,
}

/// Child transcript bytes are native/raw at discovery time.  Keep `Debug`
/// content-free so an error or span cannot stringify a full provider file.
impl fmt::Debug for DiscoveredSubagentContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredSubagentContent")
            .field("provider_kind", &self.provider_kind)
            .field("source_key_len", &self.source_key.len())
            .field(
                "legacy_source_key_present",
                &self.legacy_source_key.is_some(),
            )
            .field("byte_len", &self.bytes.len())
            .field("malformed_lines", &self.malformed_lines)
            .field("partial", &self.partial)
            .field(
                "stable_subagent_id_present",
                &self.stable_subagent_id.is_some(),
            )
            .finish()
    }
}

/// Bounded discovery result. On success, `bytes_read` measures provider IO,
/// including metadata probes. After a failed stored read it carries the
/// reserved allowance, not a measured byte count. Historical ingress charges
/// once before IO and refunds unused allowance only on successful discovery;
/// later persistence failure never refunds the remaining charge.
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
    pub(crate) usage_enumeration: Option<SubagentUsageEnumeration>,
    pub(crate) availability: super::observed_agents::live_capture::SubagentDiscoveryAvailability,
}

impl SubagentDiscovery {
    /// A failed stored read has no refundable measurement. Carry its charged
    /// allowance and a fixed partial diagnosis, never provider stderr.
    pub(crate) fn charged_stored_unavailable(byte_budget: u64) -> Self {
        Self {
            bytes_read: byte_budget,
            incomplete: true,
            availability:
                super::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable,
            warning: Some(
                "stored subagent discovery is unavailable; child attribution is incomplete"
                    .to_string(),
            ),
            ..Self::default()
        }
    }

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
    pub(crate) fn observed_kind(&self) -> Option<crate::internal::ai::observed_agents::AgentKind> {
        crate::internal::ai::observed_agents::AgentKind::from_db_str(&self.provider_kind)
    }

    /// Whether this provider participates in the existing parent aggregate.
    /// Codex child usage attribution is separate work; its independently
    /// durable content must not be counted again by the legacy parent parser.
    pub(crate) fn parent_extraction_enabled(&self) -> bool {
        self.provider_kind != "codex"
    }

    /// Consume this securely discovered child source at the one boundary
    /// where parent extraction needs its content. The runtime receives only
    /// the resulting snapshot, whose transcript is already redacted; it can
    /// never borrow the native child bytes directly.
    pub(crate) fn into_parent_extraction_snapshot(
        self,
        parent_session_id: &str,
        adapter: &dyn crate::internal::ai::observed_agents::ObservedAgent,
        policy: CaptureSnapshotPolicy,
    ) -> Option<CaptureSnapshot> {
        if !self.parent_extraction_enabled() {
            return None;
        }
        let agent_kind = self.provider_kind.clone();
        let auth = ExportAuthorized::issue_discovered_subagent(
            &agent_kind,
            parent_session_id,
            &self.bytes,
        );
        Some(CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes {
                bytes: self.bytes,
                auth,
            },
            adapter,
            crate::internal::ai::capture::snapshot::AuthorizedReasoningSourceContext::parent_only_child(
                &agent_kind, parent_session_id,
            ),
            policy,
        ))
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        provider_kind: &str,
        source_key: &str,
        bytes: &[u8],
        stable_subagent_id: Option<&str>,
    ) -> Self {
        Self {
            provider_kind: provider_kind.to_string(),
            source_key: source_key.to_string(),
            legacy_source_key: None,
            bytes: bytes.to_vec(),
            malformed_lines: 0,
            partial: false,
            stable_subagent_id: stable_subagent_id.map(str::to_string),
            codex_binding: None,
        }
    }

    #[cfg(test)]
    #[cfg(feature = "test-live-agent")]
    pub(crate) fn fixture_codex_child(
        root: &str,
        parent: &str,
        child: &str,
        capture_session: &str,
        bytes: &[u8],
    ) -> Self {
        let mut source = Self::fixture(
            "codex",
            &codex_child_logical_source_key(root, parent, child),
            bytes,
            Some(child),
        );
        source.codex_binding = Some(CodexChildBinding {
            root: root.to_owned(),
            parent: parent.to_owned(),
            child: child.to_owned(),
            capture_session: capture_session.to_owned(),
            hook_turn: None,
        });
        source
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentCaptureSummary {
    pub discovered: usize,
    pub checkpoints_written: usize,
    pub skipped_unchanged: usize,
    pub skipped_inflight: usize,
    pub partial_sources: usize,
    association_proofs: Vec<SubagentAssociationProof>,
    child_requirements: Vec<ChildCheckpointRequirement>,
    usage_candidates: Vec<CodexUsageCandidate>,
    usage_candidates_incomplete: bool,
    // Only an explicit source-owner enumeration proof can set this. Legacy
    // helper responses and a nonempty list are not exhaustive discovery.
    usage_enumeration: Option<SubagentUsageEnumeration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UsageEnumerationSelection {
    Parent,
    Import,
    Recovery,
    Boundary,
}

/// Optional source-owner evidence. It does not authorize reads or change the
/// independently retained child data. Request bindings never enter the wire.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentUsageEnumeration {
    selection: UsageEnumerationSelection,
    exhaustive: bool,
    #[serde(skip)]
    capture_session: String,
    #[serde(skip)]
    current_thread: String,
}

impl fmt::Debug for SubagentUsageEnumeration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubagentUsageEnumeration")
            .field("selection", &self.selection)
            .field("exhaustive", &self.exhaustive)
            .finish_non_exhaustive()
    }
}

impl SubagentUsageEnumeration {
    fn from_query(query: &CodexChildDiscoveryQuery, exhaustive: bool) -> Self {
        Self {
            selection: query.usage_selection(),
            exhaustive: exhaustive
                && !matches!(
                    query.selection,
                    CodexChildDiscoverySelection::Boundary { .. }
                ),
            capture_session: query.capture_session.clone(),
            current_thread: query.expected_id().to_owned(),
        }
    }

    fn matches_query(&self, query: &CodexChildDiscoveryQuery) -> bool {
        self.selection == query.usage_selection()
            && self.capture_session == query.capture_session
            && self.current_thread == query.expected_id()
    }

    fn matches_attribution(&self, input: &SubagentUsageAttributionInput<'_>) -> bool {
        self.capture_session == input.capture_session
            && self.current_thread == input.current_thread
            && matches!(
                (self.selection, input.as_of),
                (UsageEnumerationSelection::Parent, "live_capture")
                    | (UsageEnumerationSelection::Import, "import_pass")
            )
    }
}

pub(crate) struct SubagentUsageAttributionInput<'a> {
    pub scope: &'a CaptureScope,
    pub capture_session: &'a str,
    pub current_thread: &'a str,
    pub verified_cwd: &'a Path,
    pub agent_kind: &'a str,
    pub as_of: &'a str,
    pub parent_extraction: &'a serde_json::Value,
    pub parent_snapshot_complete: bool,
    pub deadline: Option<CaptureCommitDeadline>,
}

/// Prove the small metadata role chain only for unseen claims. A key prefix
/// never proves an old incarnation; malformed roles remain unknown.
fn codex_usage_claim_outside_current_namespace(
    row: &sea_orm::QueryResult,
    storage: &Path,
    session: &str,
    incarnation: Option<&str>,
    verifier: &super::capture::key::ExistingCaptureSourceVerifier,
    deadline: Instant,
    allowance: &mut usize,
) -> Result<bool> {
    use git_internal::internal::object::{
        ObjectTrait,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    };
    if row.try_get_by::<Option<i64>, _>("current_matches")? != Some(1) {
        return Ok(false);
    }
    let required = |name| -> Result<String> {
        row.try_get_by::<Option<String>, _>(name)?
            .context("child usage metadata role is missing")
    };
    let key = required("source_key")?;
    if !is_subagent_source_commitment_v2(&key) {
        return Ok(false);
    }
    let checkpoint = required("checkpoint_id")?;
    if checkpoint.len() < 3 || !checkpoint.is_ascii() {
        return Ok(false);
    }
    let parse = |value: &str| super::util::parse_repo_object_id(value).map_err(anyhow::Error::msg);
    let commit_oid = parse(&required("traces_commit")?)?;
    let tree_oid = parse(&required("tree_oid")?)?;
    let metadata_oid = parse(&required("metadata_oid")?)?;
    let mut read = |oid: &git_internal::hash::ObjectHash, kind| -> Result<Vec<u8>> {
        ensure_before_deadline(Some(deadline))?;
        let cap = allowance.saturating_sub(1).min(64 * 1024);
        if cap == 0 {
            bail!("child usage metadata allowance exhausted");
        }
        let result = if kind == "commit" {
            crate::utils::object::open_local_loose_object_no_follow(storage, oid)
                .map_err(git_internal::errors::GitError::from)
                .and_then(|file| {
                    crate::utils::object::read_git_object_bounded_validated_file(
                        file, oid, cap as u64,
                    )
                })
                .and_then(|(actual, bytes)| {
                    if actual != "commit" {
                        return Err(git_internal::errors::GitError::InvalidObjectInfo(
                            "child usage role is not a commit".to_owned(),
                        ));
                    }
                    Ok((bytes, false))
                })
        } else {
            super::checkpoint_reader::read_checkpoint_object_bounded_as(
                storage, oid, cap as u64, kind,
            )
        };
        // Failed/truncated reads consume their full allowance; never refund IO.
        let charge = result
            .as_ref()
            .map_or(cap + 1, |(bytes, _)| bytes.len() + 1);
        *allowance = allowance.saturating_sub(charge);
        ensure_before_deadline(Some(deadline))?;
        let (bytes, truncated) = result.context("read bounded child usage metadata role")?;
        if truncated {
            bail!("child usage metadata role exceeds its bounded allowance");
        }
        Ok(bytes)
    };
    let commit = Commit::from_bytes(&read(&commit_oid, "commit")?, commit_oid)?;
    if commit.tree_id != tree_oid {
        return Ok(false);
    }
    let mut oid = tree_oid;
    for name in [
        "checkpoint",
        &checkpoint[..2],
        &checkpoint[2..],
        "metadata.json",
    ] {
        let tree = Tree::from_bytes(&read(&oid, "tree")?, oid)?;
        let mut matching = tree.tree_items.iter().filter(|item| item.name == name);
        let Some(item) = matching.next() else {
            return Ok(false);
        };
        if matching.next().is_some() {
            return Ok(false);
        }
        let expected = if name == "metadata.json" {
            TreeItemMode::Blob
        } else {
            TreeItemMode::Tree
        };
        if item.mode != expected {
            return Ok(false);
        }
        oid = item.id;
    }
    if oid != metadata_oid {
        return Ok(false);
    }
    let metadata: serde_json::Value = serde_json::from_slice(&read(&metadata_oid, "blob")?)?;
    let subagent = &metadata["subagent"];
    if metadata["checkpoint_id"] != checkpoint
        || metadata["session_id"] != session
        || metadata["agent_kind"] != "codex"
        || metadata["scope"] != "subagent"
        || subagent["provenance"] != "content"
        || subagent["source_key"] != key
        || subagent["content_schema_version"] != 2
        || subagent["revision"].as_i64() != row.try_get_by::<Option<i64>, _>("current_revision")?
    {
        return Ok(false);
    }
    let identity = &subagent["source_identity"];
    let field = |name| -> Result<&str> {
        identity[name]
            .as_str()
            .filter(|value| codex_child_identifier(value, 64))
            .context("child usage source identity is unavailable")
    };
    let root = field("root")?;
    let parent = field("parent")?;
    let child = field("child")?;
    if child == parent || child == root {
        return Ok(false);
    }
    let logical = codex_child_logical_source_key(root, parent, child);
    // Missing in older metadata: positively try the original un-namespaced
    // writer only. A mismatch cannot by itself prove a different incarnation.
    let recorded = match subagent.get("capture_incarnation") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value))
            if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(value.as_str())
        }
        _ => return Ok(false),
    };
    if recorded == incarnation {
        return Ok(false);
    }
    let preimage = subagent_source_preimage("codex", &logical, recorded)?;
    let expected = verifier.derive(CaptureSourceCommitmentDomain::SubagentSourceV2, &preimage)?;
    Ok(key == expected)
}

pub(crate) struct SubagentUsageCapture {
    pub summary: SubagentCaptureSummary,
    pub deadline: Option<CaptureCommitDeadline>,
}

/// This invocation's observation over a durable child, not native provenance.
/// The parent consumer must recheck currency before attributing these tokens.
#[derive(Clone, PartialEq, Eq)]
struct CodexUsageCandidate {
    scope: CaptureScope,
    capture_session: String,
    capture_incarnation: Option<String>,
    root: String,
    parent: String,
    child: String,
    source_key: String,
    content_digest: String,
    checkpoint_id: String,
    revision: i64,
    usage: CodexThreadUsage,
}

impl fmt::Debug for CodexUsageCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexUsageCandidate")
            .field("revision", &self.revision)
            .field("usage_state", &self.usage.usage_state)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SubagentAssociationProof {
    scope: CaptureScope,
    capture_session: String,
    capture_incarnation: Option<String>,
    root: String,
    child: String,
    observed_turn: String,
    source_key: String,
    content_digest: String,
    content_checkpoint_id: String,
    revision: i64,
}

impl fmt::Debug for SubagentAssociationProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubagentAssociationProof")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// The current callback's target plus proofs minted only by this source owner.
/// No path, native identifier, or transcript is exposed to the boundary writer.
#[derive(Default)]
pub(crate) struct SubagentBoundaryAssociations {
    boundary_checkpoint_id: Option<String>,
    proofs: Vec<SubagentAssociationProof>,
}

pub(crate) struct SubagentBoundaryAssociationContext<'a> {
    pub scope: &'a CaptureScope,
    pub capture_session: &'a str,
    pub boundary_checkpoint_id: &'a str,
    pub deadline: Option<CaptureCommitDeadline>,
}

impl SubagentCaptureSummary {
    pub(crate) fn with_discovery_usage(mut self, discovery: &SubagentDiscovery) -> Self {
        self.usage_enumeration = discovery.usage_enumeration.clone();
        self.usage_candidates_incomplete |= discovery.incomplete
            || discovery.warning.is_some()
            || discovery.availability
                != super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available;
        self
    }

    /// A bounded observation over this pass's already durable leaves. Failure
    /// affects only attribution; the independently captured body remains usable.
    pub(crate) async fn project_usage_attribution(
        &self,
        conn: &DatabaseConnection,
        storage: &Path,
        input: SubagentUsageAttributionInput<'_>,
    ) -> Option<serde_json::Value> {
        if input.agent_kind != "codex"
            || uuid::Uuid::parse_str(input.current_thread).is_err()
            || !matches!(input.as_of, "live_capture" | "import_pass")
        {
            return None;
        }
        let parent = CodexThreadUsage::from_parent_extraction(
            input.parent_extraction,
            input.parent_snapshot_complete,
        );
        let (current, rejected, excluded, read_complete) = match self
            .current_codex_usage_candidates(conn, storage, &input)
            .await
        {
            Ok((current, rejected, excluded)) => (current, rejected, excluded, true),
            Err(_) => (Vec::new(), self.usage_candidates.len(), 0, false),
        };
        let unknown = rejected
            + current
                .iter()
                .filter(|(candidate, _)| candidate.usage.token_usage.is_none())
                .count();
        let counters = current
            .iter()
            .filter_map(|(candidate, _)| candidate.usage.token_usage.clone())
            .collect::<Vec<_>>();
        let child = sum_codex_usage_counters(&counters);
        let complete = read_complete
            && self
                .usage_enumeration
                .as_ref()
                .is_some_and(|proof| proof.exhaustive && proof.matches_attribution(&input))
            && !self.usage_candidates_incomplete
            && self.skipped_inflight == 0
            && self.partial_sources == 0
            && rejected == 0
            && current
                .iter()
                .all(|(candidate, _)| candidate.usage.usage_state == CodexUsageState::Complete)
            && (counters.is_empty() || child.is_some());
        let aggregate = if complete && parent.usage_state == CodexUsageState::Complete {
            match (&parent.token_usage, &child) {
                (Some(parent), Some(child)) => {
                    sum_codex_usage_counters(&[parent.clone(), child.clone()])
                }
                (Some(parent), None) if current.is_empty() => Some(parent.clone()),
                _ => None,
            }
        } else {
            None
        };
        // Derived metadata uses source references. Original native identities
        // remain in the independently retained ordinary source checkpoint.
        let sources = current
            .iter()
            .map(|(candidate, depth)| {
                let parent = if candidate.parent == input.current_thread {
                    None
                } else {
                    current
                        .iter()
                        .find(|(ancestor, _)| {
                            ancestor.child == candidate.parent && ancestor.root == candidate.root
                        })
                        .map(|(ancestor, _)| ancestor.source_key.as_str())
                };
                serde_json::json!({
                    "source":candidate.source_key,"revision":candidate.revision,
                    "checkpoint_id":candidate.checkpoint_id,"parent":parent,"depth":depth,
                    "usage":candidate.usage.token_usage,"usage_state":candidate.usage.usage_state,
                    "usage_provenance":"computed_this_pass"
                })
            })
            .collect::<Vec<_>>();
        Some(serde_json::json!({
            "schema_version":1,"scope":"session_snapshot","as_of":input.as_of,
            "children_coverage":if complete {"complete"} else if current.is_empty() && rejected==0 {"unknown"} else {"partial"},
            "partial":!complete || parent.usage_state != CodexUsageState::Complete || aggregate.is_none(),
            "source_count":current.len()+rejected,"unknown_source_count":unknown,
            "excluded_legacy_count":excluded,
            "parent_token_usage":parent.token_usage,"child_token_usage":child,
            "aggregate_includes_child_usage":aggregate.is_some() && !current.is_empty(),
            "aggregate_token_usage":aggregate,"sources":sources
        }))
    }

    async fn current_codex_usage_candidates(
        &self,
        conn: &DatabaseConnection,
        storage: &Path,
        input: &SubagentUsageAttributionInput<'_>,
    ) -> Result<(Vec<(CodexUsageCandidate, usize)>, usize, usize)> {
        let original = input
            .deadline
            .context("child attribution has no original capture deadline")?;
        let now = Instant::now();
        let remaining = original
            .monotonic()
            .checked_duration_since(now)
            .context("child attribution capture deadline elapsed")?;
        let end = now
            .checked_add((remaining / 4).min(Duration::from_millis(100)))
            .context("child attribution auxiliary deadline overflow")?;
        let deadline = narrow_subagent_work_deadline(original, end)
            .context("child attribution paired deadline overflow")?
            .monotonic();
        await_subagent_precommit_read_until(Some(deadline), async {
            let txn = conn
                .begin()
                .await
                .context("open child usage read snapshot")?;
            let result = self
                .current_codex_usage_candidates_in_snapshot(&txn, storage, input, deadline)
                .await;
            txn.rollback()
                .await
                .context("release child usage read snapshot")?;
            result
        })
        .await
    }

    async fn current_codex_usage_candidates_in_snapshot(
        &self,
        txn: &DatabaseTransaction,
        storage: &Path,
        input: &SubagentUsageAttributionInput<'_>,
        deadline: Instant,
    ) -> Result<(Vec<(CodexUsageCandidate, usize)>, usize, usize)> {
        input.scope.assert_workspace_fence_live(txn).await?;
        let parent = txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT working_dir,
                    json_extract(metadata_json, '$.capture_incarnation') AS incarnation
             FROM agent_session s
             WHERE session_id=? AND agent_kind='codex' AND provider_session_id=?
               AND scope_state='scoped' AND repo_id=? AND worktree_id=?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND NOT EXISTS (SELECT 1 FROM agent_import_tombstone t
                   WHERE t.agent_kind=s.agent_kind AND t.provider_session_id=s.provider_session_id)",
            [input.capture_session.into(),input.current_thread.into(),
             input.scope.repo_id.clone().into(),input.scope.worktree_id.clone().into(),
             input.scope.workspace_id.clone().into(),input.scope.workspace_fence.into()],
        )).await.context("read scoped parent for child usage")?
            .context("child usage parent is no longer current in this capture scope")?;
        let cwd: String = parent.try_get_by("working_dir")?;
        let incarnation: Option<String> = parent.try_get_by("incarnation")?;
        if Path::new(&cwd) != input.verified_cwd {
            bail!("child usage parent worktree changed; attribution is unavailable");
        }
        // Keep invalid/missing companions in the result as unknown. Inner
        // joins would silently turn corrupted or in-flight claims into zero.
        let rows = txn.query_all_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT CASE WHEN length(c.source_key)<=256 THEN c.source_key END AS source_key,
                    CASE WHEN length(c.current_checkpoint_id)<=128 THEN c.current_checkpoint_id END AS checkpoint_id,
                    CASE WHEN length(c.current_digest)<=256 THEN c.current_digest END AS content_digest,
                    c.current_revision,
                    CASE WHEN length(cp.traces_commit)<=128 THEN cp.traces_commit END AS traces_commit,
                    CASE WHEN length(cp.tree_oid)<=128 THEN cp.tree_oid END AS tree_oid,
                    CASE WHEN length(cp.metadata_blob_oid)<=128 THEN cp.metadata_blob_oid END AS metadata_oid,
                    (c.content_schema_version=2 AND c.state='idle' AND c.owner IS NULL
                     AND c.lease_expires_at IS NULL AND c.attempt_digest IS NULL
                     AND c.attempt_checkpoint_id IS NULL AND c.current_revision>0
                     AND EXISTS (SELECT 1 FROM agent_subagent_content_revision r
                         WHERE r.parent_session_id=c.parent_session_id AND r.provider_kind=c.provider_kind
                           AND r.source_key=c.source_key AND r.content_schema_version=c.content_schema_version
                           AND r.revision=c.current_revision AND r.checkpoint_id=c.current_checkpoint_id
                           AND r.content_digest=c.current_digest)
                     AND EXISTS (SELECT 1 FROM agent_checkpoint cp WHERE cp.checkpoint_id=c.current_checkpoint_id
                         AND cp.session_id=c.parent_session_id AND cp.scope='subagent')) AS current_matches
             FROM agent_subagent_content_claim c
             LEFT JOIN agent_checkpoint cp ON cp.checkpoint_id=c.current_checkpoint_id
                 AND cp.session_id=c.parent_session_id AND cp.scope='subagent'
             WHERE c.parent_session_id=? AND c.provider_kind='codex'
             ORDER BY c.source_key,c.content_schema_version DESC LIMIT ?",
            [input.capture_session.into(), (i64::try_from(MAX_SUBAGENT_SOURCES_PER_CAPTURE)
                .context("child usage source limit exceeds the supported integer range")? + 1).into()],
        )).await.context("read bounded current child usage claims")?;
        let verifier = if rows.is_empty() {
            None
        } else {
            Some(
                super::capture::key::ExistingCaptureSourceVerifier::acquire(
                    txn,
                    input.scope,
                    storage,
                    input.verified_cwd,
                    deadline,
                )
                .await?,
            )
        };
        let mut current = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut rejected = 0;
        let mut excluded = 0;
        let mut metadata_allowance = 1024 * 1024;
        for (index, row) in rows.into_iter().enumerate() {
            ensure_before_deadline(Some(deadline))?;
            // The extra row is a saturation witness, not an expanded scan.
            if index >= MAX_SUBAGENT_SOURCES_PER_CAPTURE {
                rejected += 1;
                continue;
            }
            let key: Option<String> = row.try_get_by("source_key")?;
            let candidate = key.as_deref().and_then(|key| {
                self.usage_candidates
                    .iter()
                    .find(|candidate| candidate.source_key == key)
            });
            let Some(candidate) = candidate else {
                let outside_namespace = verifier.as_ref().is_some_and(|verifier| {
                    codex_usage_claim_outside_current_namespace(
                        &row,
                        storage,
                        input.capture_session,
                        incarnation.as_deref(),
                        verifier,
                        deadline,
                        &mut metadata_allowance,
                    )
                    .unwrap_or(false)
                });
                if outside_namespace {
                    excluded += 1;
                } else {
                    rejected += 1;
                }
                continue;
            };
            if !seen.insert(candidate.source_key.clone()) {
                rejected += 1;
                continue;
            }
            let checkpoint: Option<String> = row.try_get_by("checkpoint_id")?;
            let digest: Option<String> = row.try_get_by("content_digest")?;
            let revision: Option<i64> = row.try_get_by("current_revision")?;
            let matches: Option<i64> = row.try_get_by("current_matches")?;
            let depth = codex_usage_candidate_depth(
                candidate,
                &self.usage_candidates,
                input.current_thread,
            );
            if self
                .usage_candidates
                .iter()
                .filter(|other| other.root == candidate.root && other.child == candidate.child)
                .count()
                != 1
                || candidate.scope != *input.scope
                || candidate.capture_session != input.capture_session
                || candidate.capture_incarnation != incarnation
                || depth.is_none()
                || checkpoint.as_deref() != Some(&candidate.checkpoint_id)
                || digest.as_deref() != Some(&candidate.content_digest)
                || revision != Some(candidate.revision)
                || matches != Some(1)
                || candidate.usage.clone().checked().is_none()
            {
                rejected += 1;
                continue;
            }
            let preimage = subagent_source_preimage(
                "codex",
                &codex_child_logical_source_key(
                    &candidate.root,
                    &candidate.parent,
                    &candidate.child,
                ),
                incarnation.as_deref(),
            )?;
            let expected = verifier
                .as_ref()
                .context("child usage verifier is unavailable")?
                .derive(CaptureSourceCommitmentDomain::SubagentSourceV2, &preimage)?;
            let link = txn
                .query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 AS present FROM agent_subagent_link WHERE parent_session_id=?
                 AND content_checkpoint_id=? AND stable_subagent_id=? LIMIT 1",
                    [
                        input.capture_session.into(),
                        candidate.checkpoint_id.clone().into(),
                        candidate.child.clone().into(),
                    ],
                ))
                .await
                .context("recheck current child usage link")?;
            if expected != candidate.source_key || link.is_none() {
                rejected += 1;
                continue;
            }
            if let Some(depth) = depth {
                current.push((candidate.clone(), depth));
            }
        }
        rejected += self
            .usage_candidates
            .iter()
            .filter(|candidate| !seen.contains(&candidate.source_key))
            .count();
        // An ancestor's presence in the incoming batch is insufficient: its
        // own current claim must have survived the same snapshot checks.
        let proven = current
            .iter()
            .map(|(candidate, _)| candidate.clone())
            .collect::<Vec<_>>();
        let before = current.len();
        current.retain(|(candidate, _)| {
            codex_usage_candidate_depth(candidate, &proven, input.current_thread).is_some()
        });
        rejected += before - current.len();
        current.sort_by(|a, b| a.0.source_key.cmp(&b.0.source_key));
        ensure_before_deadline(Some(deadline))?;
        Ok((current, rejected, excluded))
    }

    /// Preserve durable counts after an interrupted batch without advertising
    /// a successful association attempt. A fresh callback can re-prove it.
    pub(crate) fn without_association_proofs(mut self) -> Self {
        self.association_proofs.clear();
        self.usage_enumeration = None;
        self.usage_candidates_incomplete |= !self.usage_candidates.is_empty();
        self
    }

    pub(crate) fn bind_boundary_associations(
        &self,
        event_kind: super::hooks::LifecycleEventKind,
        event_id: uuid::Uuid,
        context: &super::observed_agents::live_capture::LiveCaptureContext<'_>,
        hints: &super::observed_agents::live_capture::UntrustedChildSourceHints,
    ) -> SubagentBoundaryAssociations {
        use super::observed_agents::live_capture::ChildSourceHintRole;
        if event_kind != super::hooks::LifecycleEventKind::SubagentEnd {
            return SubagentBoundaryAssociations::default();
        }
        let Some(hint) = hints.candidate() else {
            return SubagentBoundaryAssociations::default();
        };
        if hint.role != ChildSourceHintRole::ChildBoundary {
            return SubagentBoundaryAssociations::default();
        }
        let proofs = self
            .association_proofs
            .iter()
            .filter(|proof| {
                proof.capture_session == context.libra_session_id
                    && proof.root == context.provider_session_id
                    && Some(proof.child.as_str()) == hint.child_id
                    && Some(proof.observed_turn.as_str()) == hint.turn_id
            })
            .cloned()
            .collect();
        SubagentBoundaryAssociations {
            boundary_checkpoint_id: Some(
                super::capture::checkpoint::checkpoint_id_for_capture_action(
                    event_id,
                    super::capture::state::CheckpointWrite::SubagentBoundary,
                ),
            ),
            proofs,
        }
    }
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

/// Whether a child-content write stopped only because it reached its bounded
/// child slice. The parent can safely continue as partial in this case; other
/// child failures retain their fail-closed behavior.
pub(crate) fn capture_deadline_exhausted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<SubagentCaptureDeadline>()
            || matches!(
                cause.downcast_ref::<CaptureCatalogError>(),
                Some(CaptureCatalogError::DeadlineExceeded)
            )
    })
}

fn ensure_before_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(SubagentCaptureDeadline.into());
    }
    Ok(())
}

/// A reservation probe that has already observed a live owner can still miss
/// the caller's budget inside the next SQLite wait. Keep that overrun
/// retryable as writer contention; other probe failures stay unchanged.
fn live_writer_retry(error: anyhow::Error) -> anyhow::Error {
    if error
        .chain()
        .any(|cause| cause.downcast_ref::<SubagentCaptureDeadline>().is_some())
    {
        error.context(SUBAGENT_LIVE_WRITER_RETRY)
    } else {
        error
    }
}

fn normalize_subagent_marker_deadline(error: anyhow::Error) -> anyhow::Error {
    if error
        .chain()
        .any(|cause| cause.is::<crate::internal::ai::traces::TracesMarkerDeadlineExceeded>())
    {
        return SubagentCaptureDeadline.into();
    }
    error
}

/// Releasing a reservation is the narrow cleanup exception to capture's
/// deadline: it can only clear the caller's already-fenced provisional row,
/// never create or refresh a claim/link/marker. Give that recovery write one
/// short paired deadline after expiry so a failed capture does not strand its
/// own durable lease indefinitely. A live foreground deadline remains a
/// ceiling in both clock domains: its SQLite authorization must never be
/// silently replaced by a fresh recovery wall-clock deadline.
fn subagent_cleanup_deadline(deadline: CaptureCommitDeadline) -> Result<CaptureCommitDeadline> {
    if deadline.monotonic() <= Instant::now() {
        return CaptureCommitDeadline::from_budget(SUBAGENT_RESERVATION_RELEASE_GRACE)
            .context("establish subagent reservation-release recovery deadline");
    }

    let recovery_deadline = CaptureCommitDeadline::from_budget(SUBAGENT_RESERVATION_RELEASE_GRACE)
        .context("establish subagent reservation-release recovery deadline")?;
    Ok(CaptureCommitDeadline::from_established_pair(
        deadline.monotonic().min(recovery_deadline.monotonic()),
        deadline
            .sqlite_not_after_millis()
            .min(recovery_deadline.sqlite_not_after_millis()),
    ))
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

#[cfg(test)]
async fn subagent_discovery_test_pause_before_parent_validation(deadline: Instant) {
    let Some(delay) = TEST_SUBAGENT_PARENT_VALIDATION_DELAY
        .try_with(|configured| *configured)
        .ok()
        .flatten()
    else {
        return;
    };
    // When the test delay meets or exceeds the discovery window, sleep only
    // until that window so the following deadline check can preserve the
    // parent with its reserved persistence budget intact.
    let until = deadline.saturating_duration_since(Instant::now());
    tokio::time::sleep(delay.min(until)).await;
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

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_PARENT_VALIDATION_DELAY: Option<Duration>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM: Option<PathBuf>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP: Option<u64>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM: Option<PathBuf>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_OVERRIDE: Option<SubagentDiscovery>;
}

// Test-only observation point at the final history append boundary. It proves
// that the compatibility wrapper's bounded fallback is propagated past
// preparation and reservation into the actual durable checkpoint sink.
#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_FINAL_APPEND_DEADLINE: Arc<std::sync::Mutex<Option<Option<CaptureCommitDeadline>>>>;
}

// Library unit fixtures intentionally run without the CLI-owned private
// checkpoint helper. Preserve their established synchronous fixture mode
// unless a regression explicitly asks to exercise the bounded append path.
#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE: bool;
}

#[cfg(test)]
pub(crate) async fn with_subagent_parent_validation_delay<F>(
    delay: Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_PARENT_VALIDATION_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn with_subagent_discovery_helper_program<F>(
    program: PathBuf,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM
        .scope(Some(program), future)
        .await
}

#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
async fn with_subagent_discovery_helper_output_cap<F>(cap: u64, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
        .scope(Some(cap), future)
        .await
}

#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
async fn with_subagent_projection_helper_program<F>(program: PathBuf, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM
        .scope(Some(program), future)
        .await
}

#[cfg(test)]
pub(crate) async fn with_subagent_discovery_override<F>(
    discovery: SubagentDiscovery,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_OVERRIDE
        .scope(Some(discovery), future)
        .await
}

#[cfg(test)]
fn observe_subagent_final_append_deadline(deadline: Option<CaptureCommitDeadline>) {
    let _ = TEST_SUBAGENT_FINAL_APPEND_DEADLINE.try_with(|observed| {
        *observed
            .lock()
            .expect("lock final subagent append deadline observation") = Some(deadline);
    });
}

#[cfg(not(test))]
fn observe_subagent_final_append_deadline(_deadline: Option<CaptureCommitDeadline>) {}

#[cfg(test)]
fn final_subagent_append_deadline(
    caller_deadline: Option<CaptureCommitDeadline>,
    effective_deadline: Option<CaptureCommitDeadline>,
) -> Option<CaptureCommitDeadline> {
    let force_bounded = TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE
        .try_with(|force| *force)
        .unwrap_or(false);
    if caller_deadline.is_none()
        && crate::internal::ai::authorized_read::helper_program().is_none()
        && !force_bounded
    {
        return None;
    }
    effective_deadline
}

#[cfg(not(test))]
fn final_subagent_append_deadline(
    _caller_deadline: Option<CaptureCommitDeadline>,
    effective_deadline: Option<CaptureCommitDeadline>,
) -> Option<CaptureCommitDeadline> {
    effective_deadline
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum CodexChildDiscoverySelection {
    Boundary {
        root: String,
        child_id: String,
        child_path: String,
        parent_path: Option<String>,
        turn_id: Option<String>,
    },
    Parent {
        root: String,
        current_path: Option<String>,
    },
    Import {
        current: String,
        current_path: Option<String>,
    },
    Recovery {
        root: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexChildDiscoveryQuery {
    capture_session: String,
    selection: CodexChildDiscoverySelection,
}

impl CodexChildDiscoveryQuery {
    fn usage_selection(&self) -> UsageEnumerationSelection {
        match self.selection {
            CodexChildDiscoverySelection::Parent { .. } => UsageEnumerationSelection::Parent,
            CodexChildDiscoverySelection::Import { .. } => UsageEnumerationSelection::Import,
            CodexChildDiscoverySelection::Recovery { .. } => UsageEnumerationSelection::Recovery,
            CodexChildDiscoverySelection::Boundary { .. } => UsageEnumerationSelection::Boundary,
        }
    }

    fn expected_id(&self) -> &str {
        match &self.selection {
            CodexChildDiscoverySelection::Boundary { root, .. }
            | CodexChildDiscoverySelection::Parent { root, .. }
            | CodexChildDiscoverySelection::Recovery { root } => root,
            CodexChildDiscoverySelection::Import { current, .. } => current,
        }
    }

    fn current_path(&self) -> Option<&Path> {
        match &self.selection {
            CodexChildDiscoverySelection::Boundary { parent_path, .. } => parent_path.as_deref(),
            CodexChildDiscoverySelection::Parent { current_path, .. }
            | CodexChildDiscoverySelection::Import { current_path, .. } => current_path.as_deref(),
            CodexChildDiscoverySelection::Recovery { .. } => None,
        }
        .map(Path::new)
    }

    fn validate(&self) -> Result<()> {
        if !codex_child_identifier(&self.capture_session, 128)
            || !codex_child_identifier(self.expected_id(), 64)
        {
            bail!("Codex child discovery has invalid bounded identity metadata");
        }
        let valid_path = |path: &str| {
            path.len() <= 4096
                && !path.contains('\0')
                && Path::new(path).is_absolute()
                && Path::new(path).components().all(|component| {
                    matches!(
                        component,
                        Component::Normal(_) | Component::RootDir | Component::Prefix(_)
                    )
                })
        };
        if self
            .current_path()
            .is_some_and(|path| !path.to_str().is_some_and(valid_path))
        {
            bail!("Codex child discovery has an invalid bounded locator");
        }
        if let CodexChildDiscoverySelection::Boundary {
            child_id,
            child_path,
            turn_id,
            ..
        } = &self.selection
            && (!codex_child_identifier(child_id, 64)
                || !valid_path(child_path)
                || turn_id
                    .as_deref()
                    .is_some_and(|id| !codex_child_identifier(id, 64)))
        {
            bail!("Codex child discovery has invalid bounded boundary metadata");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct SubagentDiscoveryHelperRequest {
    cwd_base64: String,
    provider_session_id: String,
    remaining_ms: u64,
    byte_budget: u64,
    source_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_query: Option<CodexChildDiscoveryQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_deadline_unix_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct DiscoveredSubagentContentWire {
    provider_kind: String,
    source_key: String,
    byte_len: u64,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_binding: Option<CodexChildBinding>,
}

fn discovery_wire_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum SubagentDiscoveryHelperResponse {
    Ok {
        sources: Vec<DiscoveredSubagentContentWire>,
        bytes_read: u64,
        #[serde(default, skip_serializing_if = "discovery_wire_zero")]
        metadata_bytes_read: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex_protocol: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        availability: Option<super::observed_agents::live_capture::SubagentDiscoveryAvailability>,
        warning: Option<String>,
        incomplete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage_enumeration: Option<serde_json::Value>,
    },
    Error {
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

#[cfg(windows)]
fn encode_helper_path(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    STANDARD.encode(bytes)
}

#[cfg(windows)]
fn decode_helper_path(encoded: &str) -> Result<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let bytes = STANDARD
        .decode(encoded)
        .context("decode Windows subagent discovery path")?;
    if !bytes.len().is_multiple_of(2) {
        bail!("Windows subagent discovery path has invalid UTF-16 bytes");
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

/// Resolve the registered Libra executable for every private child-content
/// helper. Never infer a sibling named `libra`: managed hooks may be installed
/// under a renamed `--binary-path`, and an embedded host must fail closed.
fn helper_program() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(Some(program)) =
        TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM.try_with(|program| program.clone())
    {
        return Some(program);
    }
    crate::internal::ai::authorized_read::helper_program()
}

#[cfg_attr(windows, allow(dead_code))]
fn subagent_discovery_helper_output_cap() -> u64 {
    #[cfg(test)]
    if let Ok(Some(cap)) = TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP.try_with(|cap| *cap) {
        return cap;
    }
    SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
}

/// Resolve the main Libra executable which owns the projection helper
/// entrypoint. Unlike the historical discovery helper, this shares the
/// process-registered program fact so a supported `--binary-path` rename is
/// still safe and an embedded host never receives a private argv token.
#[cfg_attr(windows, allow(dead_code))]
fn projection_helper_program() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(Some(program)) = TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM.try_with(Clone::clone) {
        return Some(program);
    }
    crate::internal::ai::authorized_read::helper_program()
}

fn subagent_projection_transcript_cap(raw_bytes: u64) -> u64 {
    let proportional_cap = raw_bytes
        .saturating_mul(SUBAGENT_PROJECTION_OUTPUT_NUMERATOR)
        .saturating_add(SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR.saturating_sub(1))
        / SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR;
    proportional_cap.clamp(
        SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP,
        SUBAGENT_PROJECTION_TRANSCRIPT_CAP,
    )
}

fn subagent_projection_response_cap(raw_bytes: u64) -> u64 {
    subagent_projection_transcript_cap(raw_bytes)
        .saturating_add(SUBAGENT_PROJECTION_REPORT_CAP)
        .saturating_add(SUBAGENT_PROJECTION_FRAME_HEADER_BYTES as u64)
}

fn helper_unavailable_discovery() -> SubagentDiscovery {
    SubagentDiscovery {
        availability:
            super::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable,
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
    if input.len() as u64 > SUBAGENT_DISCOVERY_HELPER_INPUT_CAP {
        bail!("subagent discovery request exceeds its bounded input cap");
    }
    let request: SubagentDiscoveryHelperRequest =
        serde_json::from_slice(input).context("decode subagent discovery helper request")?;
    if request.codex_query.is_none() && request.codex_deadline_unix_ms.is_some() {
        bail!("subagent discovery deadline metadata has no matching provider query");
    }
    if let Some(query) = request.codex_query.as_ref() {
        query.validate()?;
        if query.expected_id() != request.provider_session_id {
            bail!("Codex child discovery request has inconsistent selection metadata");
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = &request;
    #[cfg(not(any(unix, windows)))]
    let discovery = SubagentDiscovery {
        sources: Vec::new(),
        bytes_read: 0,
        warning: Some("secure subagent content discovery is unavailable on this platform".into()),
        incomplete: request.codex_query.is_some(),
    };
    #[cfg(any(unix, windows))]
    let discovery = {
        let cwd = decode_helper_path(&request.cwd_base64)?;
        let remaining_ms = if let Some(absolute) = request.codex_deadline_unix_ms {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("read Codex discovery deadline clock")?;
            request
                .remaining_ms
                .min(absolute.saturating_sub(u64::try_from(now.as_millis()).unwrap_or(u64::MAX)))
        } else {
            request.remaining_ms.max(1)
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(remaining_ms))
            .context("compute subagent discovery helper deadline")?;
        let result = match request.codex_query.as_ref() {
            Some(query) => discover_codex_subagent_contents(
                &cwd,
                query,
                deadline,
                request.byte_budget,
                request.source_limit,
            ),
            None => discover_claude_subagent_contents(
                &cwd,
                &request.provider_session_id,
                Some(deadline),
                request.byte_budget,
                request.source_limit,
            ),
        };
        match result {
            Ok(discovery) => discovery,
            Err(_) => {
                return encode_subagent_discovery_helper_error(Instant::now() >= deadline);
            }
        }
    };
    match request.codex_query.as_ref() {
        Some(query) => encode_subagent_discovery_helper_success_with_query(discovery, Some(query)),
        None => encode_subagent_discovery_helper_success(discovery),
    }
}

/// Encode a short JSON metadata header followed by direct raw source segments.
/// The header never carries provider-native bytes, while the raw tail avoids
/// retaining a second base64 expansion in the deadline-owning parent.
fn encode_subagent_discovery_helper_success(discovery: SubagentDiscovery) -> Result<Vec<u8>> {
    encode_subagent_discovery_helper_success_with_query(discovery, None)
}

fn encode_subagent_discovery_helper_success_with_query(
    discovery: SubagentDiscovery,
    query: Option<&CodexChildDiscoveryQuery>,
) -> Result<Vec<u8>> {
    let raw_bytes = discovery.sources.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(u64::try_from(source.bytes.len()).map_err(|error| {
                anyhow!("subagent discovery helper source size overflow: {error}")
            })?)
            .context("subagent discovery helper source set size overflow")
    })?;
    let metadata_bytes_read = discovery
        .bytes_read
        .checked_sub(raw_bytes)
        .context("subagent discovery helper produced inconsistent IO accounting")?;
    if discovery.bytes_read > TRANSCRIPT_READ_HARD_CAP_BYTES
        || (query.is_none() && metadata_bytes_read != 0)
    {
        bail!("subagent discovery helper produced an invalid bounded source set");
    }
    if query.is_some()
        && discovery.availability
            == super::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable
        && !discovery.incomplete
    {
        bail!("Codex child discovery returned inconsistent availability");
    }
    let mut source_keys = std::collections::HashSet::new();
    for source in &discovery.sources {
        match query {
            Some(query) => {
                validate_codex_discovery_binding(source, query)?;
                if !source_keys.insert(source.source_key.clone()) {
                    bail!("Codex child discovery returned duplicate logical sources");
                }
            }
            None if source.provider_kind == "claude_code"
                && source.codex_binding.is_none()
                && source.stable_subagent_id.is_none() => {}
            None => bail!("subagent discovery helper produced inconsistent provider metadata"),
        }
    }
    if let Some(query) = query {
        validate_codex_discovery_completeness(&discovery.sources, query, discovery.incomplete)?;
    }
    let usage_enumeration = query.and_then(|query| discovery.usage_enumeration.as_ref()
        .filter(|proof| proof.matches_query(query)).and_then(|proof| {
            let mut proof=proof.clone();
            proof.exhaustive &= !discovery.incomplete && discovery.warning.is_none()
                && discovery.availability==super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available
                && !discovery.sources.iter().any(|source|source.partial)
                && codex_discovery_graph_closed(&discovery.sources,query);
            serde_json::to_value(proof).ok()
        }));
    let response = SubagentDiscoveryHelperResponse::Ok {
        sources: discovery
            .sources
            .iter()
            .map(|source| {
                Ok(DiscoveredSubagentContentWire {
                    provider_kind: source.provider_kind.clone(),
                    source_key: source.source_key.clone(),
                    byte_len: u64::try_from(source.bytes.len())
                        .context("subagent discovery helper source size exceeds wire range")?,
                    malformed_lines: source.malformed_lines,
                    partial: source.partial,
                    stable_subagent_id: source.stable_subagent_id.clone(),
                    codex_binding: source.codex_binding.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        bytes_read: discovery.bytes_read,
        metadata_bytes_read,
        codex_protocol: query.map(|_| 1),
        availability: query.map(|_| discovery.availability),
        warning: discovery.warning,
        incomplete: discovery.incomplete,
        usage_enumeration,
    };
    let mut frame = encode_subagent_discovery_helper_header(&response)?;
    let raw_capacity = usize::try_from(raw_bytes)
        .context("subagent discovery helper source set exceeds addressable memory")?;
    frame
        .try_reserve_exact(raw_capacity)
        .context("allocate bounded subagent discovery helper response")?;
    for source in discovery.sources {
        frame.extend_from_slice(&source.bytes);
    }
    if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP {
        bail!("subagent discovery helper response exceeds its bounded output cap");
    }
    Ok(frame)
}

fn encode_subagent_discovery_helper_error(deadline_exceeded: bool) -> Result<Vec<u8>> {
    encode_subagent_discovery_helper_header(&SubagentDiscoveryHelperResponse::Error {
        deadline_exceeded,
    })
}

fn encode_subagent_discovery_helper_header(
    response: &SubagentDiscoveryHelperResponse,
) -> Result<Vec<u8>> {
    let header =
        serde_json::to_vec(response).context("encode subagent discovery helper metadata header")?;
    let header_len = u64::try_from(header.len())
        .context("subagent discovery helper metadata header length overflow")?;
    if header_len > SUBAGENT_DISCOVERY_HELPER_HEADER_CAP {
        bail!("subagent discovery helper metadata header exceeds its bounded cap");
    }
    let header_len = u32::try_from(header.len())
        .context("subagent discovery helper metadata header exceeds wire range")?;
    let capacity = SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
        .checked_add(header.len())
        .context("subagent discovery helper metadata frame length overflow")?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(capacity)
        .context("allocate bounded subagent discovery helper metadata frame")?;
    frame.extend_from_slice(&header_len.to_le_bytes());
    frame.extend_from_slice(&header);
    Ok(frame)
}

fn decode_subagent_discovery_helper_frame(
    output: Vec<u8>,
    byte_budget: u64,
    source_limit: usize,
    deadline: Instant,
) -> Result<SubagentDiscovery> {
    decode_subagent_discovery_helper_frame_with_query(
        output,
        byte_budget,
        source_limit,
        deadline,
        None,
    )
}

fn decode_subagent_discovery_helper_frame_with_query(
    output: Vec<u8>,
    byte_budget: u64,
    source_limit: usize,
    deadline: Instant,
    query: Option<(&CodexChildDiscoveryQuery, &Path)>,
) -> Result<SubagentDiscovery> {
    if let Some((query, _)) = query {
        query.validate()?;
    }
    if output.len() < SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES {
        bail!("bounded subagent discovery helper returned an invalid response");
    }
    let header_len = u32::from_le_bytes([output[0], output[1], output[2], output[3]]) as usize;
    if u64::try_from(header_len)
        .context("bounded subagent discovery helper metadata header length overflow")?
        > SUBAGENT_DISCOVERY_HELPER_HEADER_CAP
    {
        bail!("bounded subagent discovery helper returned an invalid response");
    }
    let header_end = SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
        .checked_add(header_len)
        .filter(|end| *end <= output.len())
        .ok_or_else(|| anyhow!("bounded subagent discovery helper returned an invalid response"))?;
    let response: SubagentDiscoveryHelperResponse =
        serde_json::from_slice(&output[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end])
            .context("decode bounded subagent discovery metadata header")?;
    ensure_before_discovery_deadline(deadline)?;
    match response {
        SubagentDiscoveryHelperResponse::Ok {
            sources,
            bytes_read,
            metadata_bytes_read,
            codex_protocol,
            availability,
            warning,
            incomplete,
            usage_enumeration,
        } => {
            use super::observed_agents::live_capture::SubagentDiscoveryAvailability;
            if query.is_none() && availability.is_some() {
                bail!("legacy child discovery returned provider-specific availability");
            }
            let availability = availability.unwrap_or_else(|| {
                if query.is_some() && incomplete && sources.is_empty() {
                    SubagentDiscoveryAvailability::Unavailable
                } else {
                    SubagentDiscoveryAvailability::Available
                }
            });
            if availability == SubagentDiscoveryAvailability::Unavailable && !incomplete {
                bail!("bounded child discovery returned inconsistent availability");
            }
            if codex_protocol != query.map(|_| 1) {
                bail!(
                    "bounded subagent discovery helper did not acknowledge its provider protocol"
                );
            }
            let effective_source_limit = if query.is_some() {
                source_limit.min(MAX_SUBAGENT_SOURCES_PER_CAPTURE)
            } else {
                source_limit
            };
            if sources.len() > effective_source_limit
                || warning.as_ref().is_some_and(|value| value.len() > 4_096)
            {
                bail!("bounded subagent discovery helper exceeded its response limits");
            }
            let raw_bytes = sources.iter().try_fold(0_u64, |total, source| {
                total
                    .checked_add(source.byte_len)
                    .context("bounded subagent discovery helper byte count overflow")
            })?;
            let raw_end = header_end
                .checked_add(usize::try_from(raw_bytes).context(
                    "bounded subagent discovery helper raw frame exceeds addressable memory",
                )?)
                .filter(|end| *end == output.len())
                .ok_or_else(|| {
                    anyhow!("bounded subagent discovery helper returned an invalid response")
                })?;
            debug_assert_eq!(raw_end, output.len());
            if (query.is_none() && raw_bytes != bytes_read)
                || raw_bytes.checked_add(metadata_bytes_read) != Some(bytes_read)
                || (query.is_none() && metadata_bytes_read != 0)
                || bytes_read > byte_budget.min(TRANSCRIPT_READ_HARD_CAP_BYTES)
            {
                bail!("bounded subagent discovery helper returned an inconsistent byte count");
            }
            let mut decoded = Vec::with_capacity(sources.len());
            let mut source_keys = std::collections::HashSet::new();
            let mut offset = header_end;
            for source in sources {
                ensure_before_discovery_deadline(deadline)?;
                if query.is_none()
                    && (source.provider_kind != "claude_code"
                        || source.stable_subagent_id.is_some()
                        || source.codex_binding.is_some())
                {
                    bail!("bounded subagent discovery helper returned invalid Claude attribution");
                }
                validate_source_identity(&source.provider_kind, &source.source_key)?;
                let byte_len = usize::try_from(source.byte_len).context(
                    "bounded subagent discovery helper raw source exceeds addressable memory",
                )?;
                let end = offset
                    .checked_add(byte_len)
                    .filter(|end| *end <= output.len())
                    .ok_or_else(|| {
                        anyhow!("bounded subagent discovery helper returned an invalid response")
                    })?;
                let max_malformed_lines = byte_len.saturating_add(1);
                if source.malformed_lines > max_malformed_lines
                    || (source.malformed_lines > 0 && !source.partial)
                    || (byte_len == 0 && !source.partial)
                {
                    bail!(
                        "bounded subagent discovery helper returned inconsistent completeness metadata"
                    );
                }
                // Keep one bounded raw pipe frame while copying each segment
                // into its durable discovery owner. At most 16 MiB of output
                // plus 16 MiB of source Vecs coexist, below ACF-03's 2.5x cap.
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(byte_len)
                    .context("allocate bounded subagent transcript bytes")?;
                bytes.extend_from_slice(&output[offset..end]);
                offset = end;
                let source = DiscoveredSubagentContent {
                    provider_kind: source.provider_kind,
                    source_key: source.source_key,
                    legacy_source_key: None,
                    bytes,
                    malformed_lines: source.malformed_lines,
                    partial: source.partial,
                    stable_subagent_id: source.stable_subagent_id,
                    codex_binding: source.codex_binding,
                };
                if let Some((query, cwd)) = query {
                    validate_codex_discovery_binding(&source, query)?;
                    if !source_keys.insert(source.source_key.clone()) {
                        bail!("Codex child discovery returned duplicate logical sources");
                    }
                    let header =
                        parse_codex_discovery_header(&source.bytes, cwd, CodexHeaderMode::Strict)?;
                    let binding = source
                        .codex_binding
                        .as_ref()
                        .context("Codex child discovery response is missing source identity")?;
                    if header.id != binding.child
                        || header.root != binding.root
                        || header.parent.as_deref() != Some(binding.parent.as_str())
                    {
                        bail!("Codex child discovery response has a different source header");
                    }
                }
                decoded.push(source);
                ensure_before_discovery_deadline(deadline)?;
            }
            if let Some((query, _)) = query {
                validate_codex_discovery_completeness(&decoded, query, incomplete)?;
            }
            let usage_enumeration = usage_enumeration.and_then(|wire| {
                let (query, _) = query?;
                let parsed = serde_json::from_value::<SubagentUsageEnumeration>(wire).ok()?;
                if parsed.selection != query.usage_selection() {
                    return None;
                }
                let mut proof = SubagentUsageEnumeration::from_query(query, parsed.exhaustive);
                proof.exhaustive &= !incomplete
                    && warning.is_none()
                    && availability == SubagentDiscoveryAvailability::Available
                    && !decoded.iter().any(|source| source.partial)
                    && codex_discovery_graph_closed(&decoded, query);
                Some(proof)
            });
            Ok(SubagentDiscovery {
                sources: decoded,
                bytes_read,
                warning,
                incomplete,
                availability,
                usage_enumeration,
            })
        }
        SubagentDiscoveryHelperResponse::Error { deadline_exceeded } => {
            if header_end != output.len() {
                bail!("bounded subagent discovery helper returned an invalid response");
            }
            if deadline_exceeded {
                return Err(SubagentDiscoveryDeadline.into());
            }
            bail!("bounded subagent discovery helper reported a safe failure")
        }
    }
}

/// Killable async boundary used by every production live/import caller.
pub(crate) async fn discover_claude_subagent_contents_bounded(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Instant,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    discover_subagent_contents_bounded(
        cwd,
        provider_session_id,
        deadline,
        byte_budget,
        source_limit,
        None,
    )
    .await
}

/// Typed provider ports delegate here; this remains the only discovery IO owner.
pub(crate) async fn discover_codex_subagent_contents_stored(
    context: &super::observed_agents::live_capture::LiveCaptureContext<'_>,
    selection: super::observed_agents::live_capture::StoredSubagentSelection<'_>,
    limits: super::observed_agents::live_capture::SubagentDiscoveryLimits,
    deadline: Instant,
) -> Result<SubagentDiscovery> {
    use super::observed_agents::live_capture::StoredSubagentSelection;
    let selection = match selection {
        StoredSubagentSelection::ImportCurrent { parent_locator } => {
            CodexChildDiscoverySelection::Import {
                current: context.provider_session_id.to_string(),
                current_path: parent_locator
                    .map(|path| {
                        path.to_str()
                            .context("stored child locator is not UTF-8")
                            .map(str::to_owned)
                    })
                    .transpose()?,
            }
        }
        StoredSubagentSelection::TerminalRecoveryRoot => CodexChildDiscoverySelection::Recovery {
            root: context.provider_session_id.to_string(),
        },
    };
    let query = CodexChildDiscoveryQuery {
        capture_session: context.libra_session_id.to_string(),
        selection,
    };
    discover_subagent_contents_bounded(
        context.verified_cwd,
        context.provider_session_id,
        deadline,
        limits.byte_budget,
        limits.source_limit,
        Some(&query),
    )
    .await
}

pub(crate) async fn discover_codex_subagent_contents_live(
    context: &super::observed_agents::live_capture::LiveCaptureContext<'_>,
    hints: &super::observed_agents::live_capture::UntrustedChildSourceHints,
    deadline: Instant,
) -> Result<SubagentDiscovery> {
    use super::observed_agents::live_capture::ChildSourceHintRole;
    let path = |path: &Path| {
        path.to_str()
            .context("live child locator is not UTF-8")
            .map(str::to_owned)
    };
    let selection = match hints.candidate() {
        Some(hint) if hint.role == ChildSourceHintRole::ChildBoundary => {
            CodexChildDiscoverySelection::Boundary {
                root: context.provider_session_id.to_string(),
                child_id: hint
                    .child_id
                    .context("child boundary is missing its identifier")?
                    .to_string(),
                child_path: path(
                    hint.child_path
                        .context("child boundary is missing its locator")?,
                )?,
                parent_path: hint.current_or_parent_path.map(path).transpose()?,
                turn_id: hint.turn_id.map(str::to_owned),
            }
        }
        hint => CodexChildDiscoverySelection::Parent {
            root: context.provider_session_id.to_string(),
            current_path: hint
                .and_then(|hint| hint.current_or_parent_path)
                .map(path)
                .transpose()?,
        },
    };
    let query = CodexChildDiscoveryQuery {
        capture_session: context.libra_session_id.to_string(),
        selection,
    };
    let mut discovery = discover_subagent_contents_bounded(
        context.verified_cwd,
        context.provider_session_id,
        deadline,
        TRANSCRIPT_READ_HARD_CAP_BYTES,
        MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        Some(&query),
    )
    .await?;
    if matches!(
        hints,
        super::observed_agents::live_capture::UntrustedChildSourceHints::Unavailable
    ) {
        discovery.incomplete = true;
    }
    Ok(discovery)
}

async fn discover_subagent_contents_bounded(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Instant,
    byte_budget: u64,
    source_limit: usize,
    query: Option<&CodexChildDiscoveryQuery>,
) -> Result<SubagentDiscovery> {
    if let Some(query) = query {
        query.validate()?;
        if query.expected_id() != provider_session_id {
            bail!("Codex child discovery request has inconsistent selection metadata");
        }
    }
    #[cfg(test)]
    if let Ok(Some(discovery)) = TEST_SUBAGENT_DISCOVERY_OVERRIDE.try_with(|value| value.clone()) {
        return Ok(discovery);
    }
    #[cfg(not(any(unix, windows)))]
    {
        if query.is_some() {
            let mut discovery = helper_unavailable_discovery();
            discovery.incomplete = true;
            return Ok(discovery);
        }
        return discover_claude_subagent_contents(
            cwd,
            provider_session_id,
            Some(deadline),
            byte_budget,
            source_limit,
        );
    }

    #[cfg(any(unix, windows))]
    {
        ensure_before_discovery_deadline(deadline)?;
        let Some(helper_program) = helper_program() else {
            // An embedded host cannot establish that it understands Libra's
            // private helper argv. Fail closed for child content: blocking
            // provider files must never be opened in the async host process
            // because they cannot be killed when the command deadline expires.
            let mut discovery = helper_unavailable_discovery();
            discovery.incomplete = query.is_some();
            return Ok(discovery);
        };
        let remaining_ms = u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        let mut request = SubagentDiscoveryHelperRequest {
            cwd_base64: encode_helper_path(cwd),
            provider_session_id: provider_session_id.to_string(),
            remaining_ms,
            byte_budget,
            source_limit,
            codex_query: query.cloned(),
            codex_deadline_unix_ms: None,
        };
        let mut frame =
            serde_json::to_vec(&request).context("encode bounded subagent discovery request")?;
        if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_INPUT_CAP {
            bail!("bounded subagent discovery request exceeds its internal frame limit");
        }
        let mut command = tokio::process::Command::new(helper_program);
        command
            .arg(SUBAGENT_DISCOVERY_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        #[cfg(unix)]
        let child = command
            .spawn()
            .context("start bounded subagent discovery helper")?;
        #[cfg(unix)]
        let mut child = CancellationSafeChild::new_process_group(child);
        #[cfg(windows)]
        let mut child = CancellationSafeChild::spawn_windows(&mut command)
            .context("start contained Windows subagent helper")?;
        if query.is_some() {
            // Sample only after spawn, leaving a separate output/decode window.
            // The optional absolute bound also covers cold binary startup and
            // request decoding; the parent's monotonic kill deadline wins.
            ensure_before_discovery_deadline(deadline)?;
            let unix_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("read Codex discovery deadline clock")?;
            // Sample wall time first: a scheduler pause between these clocks
            // can only shorten the helper's absolute work window.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let transfer_ms = byte_budget
                .min(TRANSCRIPT_READ_HARD_CAP_BYTES)
                .saturating_mul(1000)
                / (64 * 1024 * 1024);
            let reserve = Duration::from_millis(100 + transfer_ms).min(remaining / 2);
            let work_remaining = remaining.saturating_sub(reserve);
            request.remaining_ms = u64::try_from(work_remaining.as_millis()).unwrap_or(u64::MAX);
            request.codex_deadline_unix_ms = Some(
                u64::try_from(unix_now.as_millis())
                    .unwrap_or(u64::MAX)
                    .saturating_add(request.remaining_ms),
            );
            frame =
                serde_json::to_vec(&request).context("encode bounded Codex discovery request")?;
            if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_INPUT_CAP {
                bail!("bounded Codex discovery request exceeds its internal frame limit");
            }
        }
        let Some(mut stdin) = child.child_mut().and_then(|child| child.stdin.take()) else {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper has no stdin pipe");
        };
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper has no stdout pipe");
        };
        let output_cap = subagent_discovery_helper_output_cap();
        let mut output_task =
            tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
        child.register_abort_on_cancel(&output_task);
        let mut request_task = tokio::spawn(async move {
            stdin.write_all(&frame).await?;
            stdin.shutdown().await
        });
        child.register_abort_on_cancel(&request_task);
        let request_result: Result<()> = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut request_task,
        )
        .await
        {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(error).context("send bounded subagent discovery request"),
            Ok(Err(error)) => Err(anyhow!(
                "bounded subagent discovery request task failed: {error}"
            )),
            Err(_) => Err(SubagentDiscoveryDeadline.into()),
        };
        if let Err(error) = request_result {
            child.terminate_and_reap();
            return Err(error);
        }
        drop(request_task);

        // A discovery helper can fork a descendant that inherits both raw
        // source pipes. Drain stdout before `wait()`, so deadline/Drop still
        // owns an unreaped group leader and can kill that descendant safely.
        let output = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut output_task,
        )
        .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                bail!("bounded subagent discovery helper returned an invalid response")
            }
            Err(_) => {
                output_task.abort();
                child.terminate_and_reap();
                return Err(SubagentDiscoveryDeadline.into());
            }
        };
        if output.len() as u64 > output_cap {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper returned an invalid response");
        }
        let status_result = match child.child_mut() {
            Some(child_process) => {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    child_process.wait(),
                )
                .await
                {
                    Ok(Ok(status)) => Ok(status),
                    Ok(Err(error)) => {
                        Err(anyhow!(error).context("wait for bounded subagent discovery helper"))
                    }
                    Err(_) => Err(SubagentDiscoveryDeadline.into()),
                }
            }
            None => Err(anyhow!("bounded subagent discovery helper was unavailable")),
        };
        let status = match status_result {
            Ok(status) => {
                // stdout reached EOF before leader reaping, so no later path
                // can signal a recycled PGID.
                child.disarm_child_after_wait();
                status
            }
            Err(error) => {
                child.terminate_and_reap();
                return Err(error);
            }
        };
        child.finish();
        if !status.success() {
            bail!("bounded subagent discovery helper returned an invalid response");
        }
        #[cfg(test)]
        subagent_discovery_test_pause_before_parent_validation(deadline).await;
        ensure_before_discovery_deadline(deadline)?;
        match query {
            Some(query) => decode_subagent_discovery_helper_frame_with_query(
                output,
                byte_budget,
                source_limit,
                deadline,
                Some((query, cwd)),
            ),
            None => {
                decode_subagent_discovery_helper_frame(output, byte_budget, source_limit, deadline)
            }
        }
    }
}

// SessionMeta also carries the native base instructions. Its raw line shares
// the projection's 1 MiB line cap; only the small identity fields are decoded.
const CODEX_CHILD_HEADER_CAP: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct CodexDiscoveryHeader {
    id: String,
    root: String,
    parent: Option<String>,
}

enum CodexHeaderMode {
    Strict,
    ChildScan,
}

#[derive(Debug, thiserror::Error)]
#[error("Codex rollout belongs to a different workspace")]
struct CodexForeignScope;

#[derive(Debug, thiserror::Error)]
#[error("Codex rollout header does not describe a child thread")]
struct CodexNotChild;

fn parse_codex_discovery_header(
    bytes: &[u8],
    cwd: &Path,
    mode: CodexHeaderMode,
) -> Result<CodexDiscoveryHeader> {
    #[derive(Deserialize)]
    struct ScopeHeader {
        #[serde(rename = "type")]
        kind: CodexChildText,
        payload: ScopePayload,
    }
    #[derive(Deserialize)]
    struct ScopePayload {
        cwd: String,
    }
    #[derive(Deserialize)]
    struct ChildProbe {
        payload: ChildProbePayload,
    }
    #[derive(Deserialize)]
    struct ChildProbePayload {
        id: CodexChildText,
        parent_thread_id: Option<serde::de::IgnoredAny>,
    }
    // Match SessionMetaLine's legacy missing-field fallback, while keeping
    // an explicitly null/wrongly typed field invalid rather than absent.
    fn present_root<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<CodexChildText>, D::Error> {
        CodexChildText::deserialize(deserializer).map(Some)
    }
    #[derive(Deserialize)]
    struct Header {
        #[serde(rename = "type")]
        kind: CodexChildText,
        payload: Payload,
    }
    #[derive(Deserialize)]
    struct Payload {
        id: CodexChildText,
        #[serde(default, deserialize_with = "present_root")]
        session_id: Option<CodexChildText>,
        parent_thread_id: Option<CodexChildText>,
        cwd: String,
    }
    let end = bytes
        .iter()
        .take(CODEX_CHILD_HEADER_CAP)
        .position(|b| *b == b'\n')
        .context("Codex child discovery has no complete bounded header")?;
    let normalize =
        super::observed_agents::transcript_source::normalize_macos_system_directory_alias;
    let scope: ScopeHeader = serde_json::from_slice(&bytes[..end])
        .context("Codex discovery has no bounded workspace header")?;
    if scope.kind.0 == "session_meta"
        && scope.payload.cwd.len() <= 4096
        && !scope.payload.cwd.contains('\0')
        && Path::new(&scope.payload.cwd).is_absolute()
        && normalize(Path::new(&scope.payload.cwd)) != normalize(cwd)
    {
        return Err(CodexForeignScope.into());
    }
    if matches!(mode, CodexHeaderMode::ChildScan)
        && scope.kind.0 == "session_meta"
        && let Ok(probe) = serde_json::from_slice::<ChildProbe>(&bytes[..end])
        && codex_child_identifier(&probe.payload.id.0, 64)
        && probe.payload.parent_thread_id.is_none()
    {
        return Err(CodexNotChild.into());
    }
    let header: Header = serde_json::from_slice(&bytes[..end])
        .context("Codex child discovery has invalid session metadata")?;
    let payload = header.payload;
    let root = match payload.session_id {
        Some(root) => root.0,
        None if payload.parent_thread_id.is_none() => payload.id.0.clone(),
        None => bail!("Codex child discovery header has no root session identity"),
    };
    if header.kind.0 != "session_meta"
        || !codex_child_identifier(&payload.id.0, 64)
        || !codex_child_identifier(&root, 64)
        || payload
            .parent_thread_id
            .as_ref()
            .is_some_and(|id| !codex_child_identifier(&id.0, 64) || id.0 == payload.id.0)
        || payload.cwd.len() > 4096
        || payload.cwd.contains('\0')
        || !Path::new(&payload.cwd).is_absolute()
        || normalize(Path::new(&payload.cwd)) != normalize(cwd)
    {
        bail!("Codex child discovery header does not match the authorized workspace");
    }
    Ok(CodexDiscoveryHeader {
        id: payload.id.0,
        root,
        parent: payload.parent_thread_id.map(|id| id.0),
    })
}

fn codex_child_logical_source_key(root: &str, parent: &str, child: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"libra-codex-child-source-v1\0");
    for value in [root, parent, child] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("source/sha256/{}", hex::encode(hash.finalize()))
}

fn validate_codex_discovery_binding(
    source: &DiscoveredSubagentContent,
    query: &CodexChildDiscoveryQuery,
) -> Result<()> {
    let binding = source
        .codex_binding
        .as_ref()
        .context("Codex child discovery response is missing bounded identity metadata")?;
    binding.validate()?;
    let selection_matches = match &query.selection {
        CodexChildDiscoverySelection::Boundary {
            root,
            child_id,
            turn_id,
            ..
        } => {
            binding.root == *root
                && binding.child == *child_id
                && (binding.hook_turn.is_none() || binding.hook_turn == *turn_id)
        }
        CodexChildDiscoverySelection::Parent { root, .. }
        | CodexChildDiscoverySelection::Recovery { root } => {
            binding.root == *root && binding.hook_turn.is_none()
        }
        CodexChildDiscoverySelection::Import { current, .. } => {
            binding.child != *current && binding.hook_turn.is_none()
        }
    };
    if !selection_matches
        || source.provider_kind != "codex"
        || binding.capture_session != query.capture_session
        || source.stable_subagent_id.as_deref() != Some(binding.child.as_str())
        || source.source_key
            != codex_child_logical_source_key(&binding.root, &binding.parent, &binding.child)
        || (source.partial && binding.hook_turn.is_some())
    {
        bail!("Codex child discovery response does not match its requested source");
    }
    Ok(())
}

fn codex_discovery_graph_closed(
    sources: &[DiscoveredSubagentContent],
    query: &CodexChildDiscoveryQuery,
) -> bool {
    if sources.len() > MAX_SUBAGENT_SOURCES_PER_CAPTURE
        || matches!(
            query.selection,
            CodexChildDiscoverySelection::Boundary { .. }
        )
    {
        return false;
    }
    let bindings = sources
        .iter()
        .map(|source| source.codex_binding.as_ref())
        .collect::<Option<Vec<_>>>();
    let Some(bindings) = bindings else {
        return false;
    };
    let mut identities = std::collections::HashSet::new();
    let mut root = None;
    for binding in &bindings {
        if binding.child == query.expected_id()
            || !identities.insert((binding.root.as_str(), binding.child.as_str()))
            || root.is_some_and(|root| root != binding.root.as_str())
        {
            return false;
        }
        root = Some(binding.root.as_str());
        let mut parent = binding.parent.as_str();
        let mut seen = std::collections::HashSet::from([binding.child.as_str()]);
        let mut reached = false;
        for _ in 0..MAX_SUBAGENT_SOURCES_PER_CAPTURE {
            if parent == query.expected_id() {
                reached = true;
                break;
            }
            if !seen.insert(parent) {
                break;
            }
            let Some(ancestor) = bindings
                .iter()
                .find(|other| other.root == binding.root && other.child == parent)
            else {
                break;
            };
            parent = &ancestor.parent;
        }
        if !reached {
            return false;
        }
    }
    true
}

fn validate_codex_discovery_completeness(
    sources: &[DiscoveredSubagentContent],
    query: &CodexChildDiscoveryQuery,
    incomplete: bool,
) -> Result<()> {
    if !incomplete
        && let CodexChildDiscoverySelection::Boundary { child_id, .. } = &query.selection
        && !sources
            .iter()
            .any(|source| source.stable_subagent_id.as_deref() == Some(child_id.as_str()))
    {
        bail!("Codex child discovery omitted an explicitly requested source");
    }
    Ok(())
}

#[cfg(any(unix, windows))]
struct CodexDiscoveryBudget {
    spent: u64,
    cap: u64,
    deadline: Instant,
}

#[cfg(any(unix, windows))]
impl CodexDiscoveryBudget {
    fn remaining(&self) -> u64 {
        self.cap.saturating_sub(self.spent)
    }

    fn read(&mut self, file: &mut std::fs::File, buffer: &mut [u8]) -> Result<usize> {
        use std::io::Read;
        ensure_before_discovery_deadline(self.deadline)?;
        let bound = buffer
            .len()
            .min(usize::try_from(self.remaining()).unwrap_or(usize::MAX));
        if bound == 0 {
            bail!("Codex child discovery exhausted its input budget");
        }
        let count = file
            .read(&mut buffer[..bound])
            .context("read pinned Codex discovery source")?;
        self.spent = self.spent.saturating_add(count as u64);
        Ok(count)
    }

    fn header(&mut self, file: &mut std::fs::File) -> Result<Vec<u8>> {
        // Read only through the first newline. Buffering ahead would read a
        // wrong-workspace transcript body before its header was authorized.
        let mut bytes = Vec::new();
        let mut byte = [0_u8; 1];
        while bytes.len() < CODEX_CHILD_HEADER_CAP {
            if self.read(file, &mut byte)? == 0 {
                break;
            }
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                return Ok(bytes);
            }
        }
        bail!("Codex child discovery has no complete bounded header")
    }
}

#[cfg(any(unix, windows))]
struct CodexPinnedDiscoveryRoot {
    path: PathBuf,
    directory: std::fs::File,
}

#[cfg(any(unix, windows))]
#[derive(Clone, PartialEq, Eq, Hash)]
struct CodexDiscoveryCandidate {
    root: usize,
    relative: PathBuf,
}

fn codex_discovery_relative_shape(relative: &Path) -> bool {
    let Some(parts) = relative
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let digit =
        |part: &str, count: usize| part.len() == count && part.bytes().all(|b| b.is_ascii_digit());
    let layout = match parts.as_slice() {
        ["sessions", year, month, day, _] => digit(year, 4) && digit(month, 2) && digit(day, 2),
        ["archived_sessions", _] => true,
        _ => false,
    };
    layout
        && relative
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("rollout-") && name.ends_with(".jsonl") && name.len() <= 255
            })
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

#[cfg(any(unix, windows))]
fn codex_discovery_hint(
    roots: &[CodexPinnedDiscoveryRoot],
    hint: &Path,
) -> Option<CodexDiscoveryCandidate> {
    let hint =
        super::observed_agents::transcript_source::normalize_macos_system_directory_alias(hint);
    roots.iter().enumerate().find_map(|(root, pinned)| {
        let relative = hint.strip_prefix(&pinned.path).ok()?;
        codex_discovery_relative_shape(relative).then(|| CodexDiscoveryCandidate {
            root,
            relative: relative.to_path_buf(),
        })
    })
}

#[cfg(any(unix, windows))]
fn open_codex_discovery_candidate(
    roots: &[CodexPinnedDiscoveryRoot],
    candidate: &CodexDiscoveryCandidate,
) -> Result<std::fs::File> {
    let root = roots
        .get(candidate.root)
        .context("Codex discovery root is unavailable")?;
    if !codex_discovery_relative_shape(&candidate.relative) {
        bail!("Codex discovery candidate has an invalid rollout layout");
    }
    open_file_beneath_pinned_provider_directory(&root.directory, &candidate.relative)
        .context("open authorized Codex rollout without following links")
}

#[cfg(any(unix, windows))]
fn codex_candidate_header(
    roots: &[CodexPinnedDiscoveryRoot],
    candidate: &CodexDiscoveryCandidate,
    cwd: &Path,
    budget: &mut CodexDiscoveryBudget,
    mode: CodexHeaderMode,
) -> Result<(std::fs::File, Vec<u8>, CodexDiscoveryHeader)> {
    let mut file = open_codex_discovery_candidate(roots, candidate)?;
    let bytes = budget.header(&mut file)?;
    let header = parse_codex_discovery_header(&bytes, cwd, mode)?;
    let path = roots[candidate.root].path.join(&candidate.relative);
    if !super::observed_agents::codex_index::valid_import_rollout_path(&path, &header.id) {
        bail!("Codex discovery rollout leaf does not match its header identity");
    }
    Ok((file, bytes, header))
}

#[cfg(any(unix, windows))]
#[derive(Clone)]
struct CodexParentDiscoveryEvidence {
    header: CodexDiscoveryHeader,
    spawned: std::collections::BTreeSet<String>,
    partial: bool,
}

#[cfg(any(unix, windows))]
fn read_codex_parent_discovery_evidence(
    roots: &[CodexPinnedDiscoveryRoot],
    candidate: &CodexDiscoveryCandidate,
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    budget: &mut CodexDiscoveryBudget,
) -> Result<CodexParentDiscoveryEvidence> {
    use std::io::{Seek, SeekFrom};
    let (mut file, header_bytes, header) =
        codex_candidate_header(roots, candidate, cwd, budget, CodexHeaderMode::Strict)?;
    let matches = match &query.selection {
        CodexChildDiscoverySelection::Import { current, .. } => header.id == *current,
        CodexChildDiscoverySelection::Boundary { root, .. } => header.root == *root,
        _ => header.id == query.expected_id() && header.root == query.expected_id(),
    };
    if !matches {
        bail!("Codex discovery parent does not match its requested selection");
    }
    if matches!(
        query.selection,
        CodexChildDiscoverySelection::Boundary { .. }
    ) {
        // A boundary needs the immediate parent's stable identity, not its
        // conversation content. Concurrent sibling activity must not invalidate
        // a separately stable child snapshot or spend its body-read allowance.
        file.seek(SeekFrom::Start(0))
            .context("recheck pinned Codex parent identity")?;
        if budget.header(&mut file)? != header_bytes {
            bail!("Codex discovery parent header changed while reading");
        }
        return Ok(CodexParentDiscoveryEvidence {
            header,
            spawned: Default::default(),
            partial: false,
        });
    }
    let before = file
        .metadata()
        .context("inspect pinned Codex parent snapshot")?;
    let mut bytes = header_bytes.clone();
    let mut buffer = [0_u8; 8192];
    let body_allowance = budget
        .remaining()
        .checked_sub(header_bytes.len() as u64)
        .context("Codex parent discovery has no budget to recheck its header")?;
    let cap = (1024 * 1024_usize).min(bytes.len().saturating_add(body_allowance as usize));
    while bytes.len() < cap {
        let allowance = buffer.len().min(cap - bytes.len());
        let count = budget.read(&mut file, &mut buffer[..allowance])?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file
        .metadata()
        .context("inspect pinned Codex parent snapshot after reading")?;
    let partial = after.len() != bytes.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok();
    file.seek(SeekFrom::Start(0))
        .context("recheck pinned Codex parent header")?;
    if budget.header(&mut file)? != header_bytes {
        bail!("Codex discovery parent header changed while reading");
    }
    codex_parent_evidence_from_bytes(header, &bytes, partial, budget.deadline)
}

#[cfg(any(unix, windows))]
fn codex_parent_evidence_from_bytes(
    header: CodexDiscoveryHeader,
    bytes: &[u8],
    mut partial: bool,
    deadline: Instant,
) -> Result<CodexParentDiscoveryEvidence> {
    let cap = bytes.len().min(1024 * 1024);
    partial |= cap != bytes.len();
    let bytes = &bytes[..cap];
    #[derive(Deserialize)]
    struct SpawnRecord {
        #[serde(rename = "type")]
        kind: CodexChildText,
        payload: SpawnPayload,
    }
    #[derive(Deserialize)]
    struct SpawnPayload {
        #[serde(rename = "type")]
        kind: Option<CodexChildText>,
        sender_thread_id: Option<CodexChildText>,
        new_thread_id: Option<CodexChildText>,
    }
    let mut spawned = std::collections::BTreeSet::new();
    for line in bytes.split_inclusive(|b| *b == b'\n').skip(1) {
        ensure_before_discovery_deadline(deadline)?;
        if !line.ends_with(b"\n") {
            partial = true;
        }
        let Ok(record) = serde_json::from_slice::<SpawnRecord>(line) else {
            partial = true;
            continue;
        };
        if record.kind.0 == "event_msg"
            && codex_child_text(&record.payload.kind) == Some("collab_agent_spawn_end")
        {
            if codex_child_text(&record.payload.sender_thread_id) != Some(header.id.as_str()) {
                partial = true;
                continue;
            }
            if let Some(id) = codex_child_text(&record.payload.new_thread_id) {
                if !codex_child_identifier(id, 64)
                    || (spawned.len() >= MAX_SUBAGENT_SOURCES_PER_CAPTURE && !spawned.contains(id))
                {
                    partial = true;
                } else {
                    spawned.insert(id.to_string());
                }
            }
        }
    }
    Ok(CodexParentDiscoveryEvidence {
        header,
        spawned,
        partial,
    })
}
#[cfg(any(unix, windows))]
fn read_codex_discovered_child(
    roots: &[CodexPinnedDiscoveryRoot],
    candidate: &CodexDiscoveryCandidate,
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    parent: Option<&CodexParentDiscoveryEvidence>,
    budget: &mut CodexDiscoveryBudget,
) -> Result<Option<DiscoveredSubagentContent>> {
    read_codex_discovered_child_with_header(roots, candidate, cwd, query, parent, budget, None)
}

#[cfg(any(unix, windows))]
fn read_codex_discovered_child_with_header(
    roots: &[CodexPinnedDiscoveryRoot],
    candidate: &CodexDiscoveryCandidate,
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    parent: Option<&CodexParentDiscoveryEvidence>,
    budget: &mut CodexDiscoveryBudget,
    expected: Option<&CodexDiscoveryHeader>,
) -> Result<Option<DiscoveredSubagentContent>> {
    use std::io::{Seek, SeekFrom};
    let explicit = match &query.selection {
        CodexChildDiscoverySelection::Boundary { child_path, .. } => {
            codex_discovery_hint(roots, Path::new(child_path)).as_ref() == Some(candidate)
        }
        _ => false,
    };
    let mode = if explicit {
        CodexHeaderMode::Strict
    } else {
        CodexHeaderMode::ChildScan
    };
    let (mut file, header_bytes, header) =
        codex_candidate_header(roots, candidate, cwd, budget, mode)?;
    if expected.is_some_and(|expected| expected != &header) {
        bail!("Codex child identity changed after its bounded header probe");
    }

    let Some(parent_id) = header.parent.as_deref() else {
        return Ok(None);
    };
    let selected = match &query.selection {
        CodexChildDiscoverySelection::Boundary { root, child_id, .. } => {
            header.root == *root && header.id == *child_id
        }
        CodexChildDiscoverySelection::Import { current, .. } => {
            // This cached header was selected by the bounded ancestor walk.
            // Missing intermediate body evidence makes it partial, not absent.
            expected.is_some()
                || parent_id == current
                || header.root == *current
                || parent.is_some_and(|parent| {
                    parent.header.id == parent_id && parent.header.root == header.root
                })
        }
        CodexChildDiscoverySelection::Parent { root, .. } => {
            header.root == *root && parent.is_none_or(|parent| parent_id == parent.header.id)
        }
        CodexChildDiscoverySelection::Recovery { root } => header.root == *root,
    };
    if !selected {
        return Ok(None);
    }
    if matches!(query.selection, CodexChildDiscoverySelection::Import { .. })
        && parent.is_some_and(|parent| parent.header.root != header.root)
    {
        return Ok(None);
    }
    let parent_matches = parent
        .is_some_and(|parent| parent.header.id == parent_id && parent.header.root == header.root);
    let mut partial = match &query.selection {
        CodexChildDiscoverySelection::Boundary { .. } => !parent_matches,
        CodexChildDiscoverySelection::Recovery { .. } => false,
        _ => {
            !parent_matches
                || parent
                    .is_some_and(|parent| parent.partial || !parent.spawned.contains(&header.id))
        }
    };
    let hook_turn = match &query.selection {
        CodexChildDiscoverySelection::Boundary { turn_id, .. } if !partial => turn_id.clone(),
        _ => None,
    };
    let mut binding = CodexChildBinding {
        root: header.root,
        parent: parent_id.to_string(),
        child: header.id,
        capture_session: query.capture_session.clone(),
        hook_turn,
    };
    binding.validate()?;
    let before = file
        .metadata()
        .context("inspect pinned Codex child snapshot")?;
    let mut bytes = header_bytes.clone();
    let mut buffer = [0_u8; 8192];
    let body_allowance = budget
        .remaining()
        .checked_sub(header_bytes.len() as u64)
        .context("Codex child discovery has no budget to recheck its header")?;
    let cap = bytes.len().saturating_add(body_allowance as usize);
    while bytes.len() < cap {
        let allowance = buffer.len().min(cap - bytes.len());
        let count = budget.read(&mut file, &mut buffer[..allowance])?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file
        .metadata()
        .context("inspect pinned Codex child snapshot after reading")?;
    partial |= after.len() != bytes.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok();
    file.seek(SeekFrom::Start(0))
        .context("recheck pinned Codex child header")?;
    if budget.header(&mut file)? != header_bytes {
        bail!("Codex child discovery header changed while reading");
    }
    let summary = summarize_codex_child_body(&bytes, &binding)?;
    if summary.content_records == 0 {
        return Ok(None);
    }
    partial |= summary.partial;
    binding.hook_turn = if partial { None } else { summary.observed_turn };
    Ok(Some(DiscoveredSubagentContent {
        provider_kind: "codex".into(),
        source_key: codex_child_logical_source_key(&binding.root, &binding.parent, &binding.child),
        legacy_source_key: None,
        bytes,
        malformed_lines: summary.malformed_lines,
        partial,
        stable_subagent_id: Some(binding.child.clone()),
        codex_binding: Some(binding),
    }))
}

#[cfg(any(unix, windows))]
fn collect_codex_discovery_candidates(
    roots: &[CodexPinnedDiscoveryRoot],
    budget: &CodexDiscoveryBudget,
) -> (Vec<CodexDiscoveryCandidate>, bool) {
    use super::observed_agents::transcript_source::{
        PinnedEntryType, open_directory_beneath_pinned_directory,
    };
    let mut candidates = Vec::new();
    let mut entries = 0_usize;
    let mut incomplete = false;
    'roots: for (index, root) in roots.iter().enumerate() {
        let mut pending = vec![
            PathBuf::from("archived_sessions"),
            PathBuf::from("sessions"),
        ];
        while let Some(relative) = pending.pop() {
            if Instant::now() >= budget.deadline {
                incomplete = true;
                break 'roots;
            }
            let directory =
                match open_directory_beneath_pinned_directory(&root.directory, &relative) {
                    Ok(directory) => directory,
                    Err(error) => {
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_none_or(|error| error.kind() != std::io::ErrorKind::NotFound)
                        {
                            incomplete = true;
                        }
                        continue;
                    }
                };
            let Ok(iter) = super::observed_agents::read_dir_pinned_provider_directory(&directory)
            else {
                incomplete = true;
                continue;
            };
            let mut child_directories = Vec::new();
            for entry in iter {
                entries += 1;
                if entries > 4096 || Instant::now() >= budget.deadline {
                    incomplete = true;
                    break 'roots;
                }
                let Ok(entry) = entry else {
                    incomplete = true;
                    continue;
                };
                let path = relative.join(entry.file_name);
                match entry.file_type {
                    PinnedEntryType::Directory if path.components().count() <= 4 => {
                        child_directories.push(path)
                    }
                    PinnedEntryType::File if codex_discovery_relative_shape(&path) => candidates
                        .push(CodexDiscoveryCandidate {
                            root: index,
                            relative: path,
                        }),
                    PinnedEntryType::Symlink | PinnedEntryType::Other => incomplete = true,
                    _ => {}
                }
            }
            child_directories.sort();
            pending.extend(child_directories);
        }
    }
    candidates.sort_by(|a, b| {
        (
            a.relative.starts_with("archived_sessions"),
            std::cmp::Reverse(&a.relative),
            a.root,
        )
            .cmp(&(
                b.relative.starts_with("archived_sessions"),
                std::cmp::Reverse(&b.relative),
                b.root,
            ))
    });
    (candidates, incomplete)
}

#[cfg(any(unix, windows))]
fn discover_codex_subagent_contents(
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    deadline: Instant,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    use super::observed_agents::{
        AgentKind, agent_for, transcript_source::configured_provider_roots,
    };
    query.validate()?;
    ensure_before_discovery_deadline(deadline)?;
    let budget = CodexDiscoveryBudget {
        spent: 0,
        cap: byte_budget.min(TRANSCRIPT_READ_HARD_CAP_BYTES),
        deadline: deadline
            .checked_sub(Duration::from_millis(50))
            .unwrap_or(deadline),
    };
    if budget.remaining() == 0 || source_limit == 0 {
        return Ok(SubagentDiscovery {
            incomplete: true,
            ..Default::default()
        });
    }
    let adapter = agent_for(AgentKind::Codex);
    let mut roots = Vec::new();
    let configured_roots = configured_provider_roots(adapter);
    let mut incomplete = configured_roots.len() > 4;
    for path in configured_roots.into_iter().take(4) {
        match open_provider_directory_for_discovery(adapter, &path) {
            Ok(Some(directory)) => roots.push(CodexPinnedDiscoveryRoot { path, directory }),
            Ok(None) => {}
            Err(_) => incomplete = true,
        }
    }
    if roots.is_empty() {
        return Ok(SubagentDiscovery {
            incomplete: true,
            availability:
                super::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable,
            ..Default::default()
        });
    }
    discover_codex_subagent_contents_in_roots(cwd, query, budget, source_limit, roots, incomplete)
}

#[cfg(any(unix, windows))]
fn discover_codex_subagent_contents_in_roots(
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    mut budget: CodexDiscoveryBudget,
    source_limit: usize,
    roots: Vec<CodexPinnedDiscoveryRoot>,
    mut incomplete: bool,
) -> Result<SubagentDiscovery> {
    query.validate()?;
    if budget.remaining() == 0 || source_limit == 0 {
        return Ok(SubagentDiscovery {
            bytes_read: budget.spent,
            incomplete: true,
            ..Default::default()
        });
    }
    let mut parent = None;
    let mut visited = std::collections::HashSet::new();
    if let Some(hint) = query.current_path() {
        if let Some(candidate) = codex_discovery_hint(&roots, hint) {
            match read_codex_parent_discovery_evidence(&roots, &candidate, cwd, query, &mut budget)
            {
                Ok(evidence) => {
                    visited.insert(candidate);
                    parent = Some(evidence);
                }
                Err(_) => incomplete = true,
            }
        } else {
            incomplete = true;
        }
    }
    // A direct boundary candidate is consumed before enumerating unrelated
    // entries, so a large directory cannot starve an already authorized child.
    let boundary_child = match &query.selection {
        CodexChildDiscoverySelection::Boundary { child_id, .. } => Some(child_id.as_str()),
        _ => None,
    };
    let direct = match &query.selection {
        CodexChildDiscoverySelection::Boundary { child_path, .. } => {
            codex_discovery_hint(&roots, Path::new(child_path))
        }
        _ => None,
    };
    if boundary_child.is_some() && direct.is_none() {
        incomplete = true;
    }
    let mut sources: Vec<DiscoveredSubagentContent> = Vec::new();
    if let Some(candidate) = direct {
        match read_codex_discovered_child(
            &roots,
            &candidate,
            cwd,
            query,
            parent.as_ref(),
            &mut budget,
        ) {
            Ok(Some(source)) => sources.push(source),
            Ok(None) | Err(_) => incomplete = true,
        }
        visited.insert(candidate);
    }
    let (mut candidates, scan_incomplete) = collect_codex_discovery_candidates(&roots, &budget);
    incomplete |= scan_incomplete;
    if parent.is_none()
        && !matches!(
            query.selection,
            CodexChildDiscoverySelection::Boundary { .. }
        )
    {
        for candidate in &candidates {
            if !candidate
                .relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("-{}.jsonl", query.expected_id())))
            {
                continue;
            }
            if let Ok(evidence) =
                read_codex_parent_discovery_evidence(&roots, candidate, cwd, query, &mut budget)
            {
                visited.insert(candidate.clone());
                parent = Some(evidence);
                break;
            }
        }
    }
    if let Some(parent) = parent.as_ref() {
        candidates.sort_by_cached_key(|candidate| {
            !parent.spawned.iter().any(|id| {
                super::observed_agents::codex_index::valid_import_rollout_path(
                    &roots[candidate.root].path.join(&candidate.relative),
                    id,
                )
            })
        });
    }
    if !matches!(
        query.selection,
        CodexChildDiscoverySelection::Boundary { .. }
    ) {
        return discover_codex_descendants_from_candidates(
            cwd,
            query,
            &roots,
            &mut budget,
            source_limit,
            candidates,
            &visited,
            parent,
            incomplete,
        );
    }
    let mut conflicted_sources = std::collections::HashSet::new();
    for candidate in candidates {
        if boundary_child.is_some_and(|id| {
            !super::observed_agents::codex_index::valid_import_rollout_path(
                &roots[candidate.root].path.join(&candidate.relative),
                id,
            )
        }) {
            continue;
        }
        if !visited.insert(candidate.clone()) {
            continue;
        }
        if Instant::now() >= budget.deadline || budget.remaining() == 0 {
            incomplete = true;
            break;
        }
        let at_source_limit = sources.len() >= source_limit.min(MAX_SUBAGENT_SOURCES_PER_CAPTURE);
        if at_source_limit
            && !sources.iter().any(|source| {
                source.stable_subagent_id.as_deref().is_some_and(|id| {
                    super::observed_agents::codex_index::valid_import_rollout_path(
                        &roots[candidate.root].path.join(&candidate.relative),
                        id,
                    )
                })
            })
        {
            incomplete = true;
            continue;
        }
        match read_codex_discovered_child(
            &roots,
            &candidate,
            cwd,
            query,
            parent.as_ref(),
            &mut budget,
        ) {
            Ok(Some(source)) => {
                if let Some(old) = sources
                    .iter_mut()
                    .find(|old| old.source_key == source.source_key)
                {
                    if source.bytes.starts_with(&old.bytes) {
                        *old = source;
                        if conflicted_sources.contains(&old.source_key) {
                            old.partial = true;
                            if let Some(binding) = old.codex_binding.as_mut() {
                                binding.hook_turn = None;
                            }
                        }
                    } else if !old.bytes.starts_with(&source.bytes) {
                        conflicted_sources.insert(old.source_key.clone());
                        old.partial = true;
                        if let Some(binding) = old.codex_binding.as_mut() {
                            binding.hook_turn = None;
                        }
                        incomplete = true;
                    }
                } else if !at_source_limit {
                    sources.push(source);
                } else {
                    incomplete = true;
                }
            }
            Ok(None) => {}
            Err(error) if error.is::<CodexForeignScope>() || error.is::<CodexNotChild>() => {
                if parent.as_ref().is_some_and(|parent| {
                    parent.spawned.iter().any(|id| {
                        super::observed_agents::codex_index::valid_import_rollout_path(
                            &roots[candidate.root].path.join(&candidate.relative),
                            id,
                        )
                    })
                }) {
                    incomplete = true;
                }
            }
            Err(_) => incomplete = true,
        }
    }
    if sources.iter().any(|source| source.partial) {
        incomplete = true;
    }
    if matches!(
        query.selection,
        CodexChildDiscoverySelection::Parent { .. } | CodexChildDiscoverySelection::Import { .. }
    ) && parent.as_ref().is_some_and(|parent| {
        parent.partial
            || parent.spawned.iter().any(|id| {
                !sources
                    .iter()
                    .any(|source| source.stable_subagent_id.as_deref() == Some(id.as_str()))
            })
    }) {
        incomplete = true;
    }
    if boundary_child.is_some_and(|id| {
        !sources
            .iter()
            .any(|source| source.stable_subagent_id.as_deref() == Some(id))
    }) {
        incomplete = true;
    }
    Ok(SubagentDiscovery {
        usage_enumeration: Some(SubagentUsageEnumeration::from_query(query,false)),
        sources, bytes_read: budget.spent, incomplete,
        warning: incomplete.then(|| "bounded Codex child discovery is incomplete; retry from the original authorized environment".into()),
        availability: super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available,
    })
}

#[cfg(any(unix, windows))]
enum CodexHeaderMembership {
    Descendant(usize),
    Outside,
    Unknown,
}

#[cfg(any(unix, windows))]
fn codex_header_membership(
    header: &CodexDiscoveryHeader,
    headers: &[(CodexDiscoveryCandidate, CodexDiscoveryHeader)],
    current: &str,
) -> CodexHeaderMembership {
    if header.id == current {
        return CodexHeaderMembership::Outside;
    }
    let Some(mut parent) = header.parent.as_deref() else {
        return CodexHeaderMembership::Outside;
    };
    let mut seen = std::collections::HashSet::from([header.id.as_str()]);
    for depth in 1..=MAX_SUBAGENT_SOURCES_PER_CAPTURE {
        if parent == current {
            return CodexHeaderMembership::Descendant(depth);
        }
        if parent == header.root && current != header.root {
            return if headers
                .iter()
                .any(|(_, root)| root.id == parent && root.parent.is_some())
            {
                CodexHeaderMembership::Unknown
            } else {
                CodexHeaderMembership::Outside
            };
        }
        if !seen.insert(parent) {
            return CodexHeaderMembership::Unknown;
        }
        let mut ancestors = headers
            .iter()
            .filter(|(_, other)| other.root == header.root && other.id == parent)
            .map(|(_, header)| header);
        let Some(ancestor) = ancestors.next() else {
            return CodexHeaderMembership::Unknown;
        };
        if ancestors.any(|other| other.parent != ancestor.parent) {
            return CodexHeaderMembership::Unknown;
        }
        let Some(next) = ancestor.parent.as_deref() else {
            return CodexHeaderMembership::Outside;
        };
        parent = next;
    }
    CodexHeaderMembership::Unknown
}

/// Process one bounded candidate enumeration. Headers establish the descendant
/// closure before any selected child body is read; no second directory walk is
/// performed. Half the remaining byte/time allowance is reserved for bodies.
#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
fn discover_codex_descendants_from_candidates(
    cwd: &Path,
    query: &CodexChildDiscoveryQuery,
    roots: &[CodexPinnedDiscoveryRoot],
    budget: &mut CodexDiscoveryBudget,
    source_limit: usize,
    candidates: Vec<CodexDiscoveryCandidate>,
    visited: &std::collections::HashSet<CodexDiscoveryCandidate>,
    parent: Option<CodexParentDiscoveryEvidence>,
    mut incomplete: bool,
) -> Result<SubagentDiscovery> {
    // Only the current parent's incomplete body qualifies ordinary capture.
    // Descendant spawn scans may hit a tighter cap than complete child bodies.
    incomplete |= matches!(
        query.selection,
        CodexChildDiscoverySelection::Parent { .. } | CodexChildDiscoverySelection::Import { .. }
    ) && parent.as_ref().is_some_and(|parent| parent.partial);
    let mut enumeration_incomplete =
        incomplete || parent.as_ref().is_none_or(|parent| parent.partial);
    let root = parent
        .as_ref()
        .map(|parent| parent.header.root.as_str())
        .or_else(|| match query.selection {
            CodexChildDiscoverySelection::Parent { .. }
            | CodexChildDiscoverySelection::Recovery { .. } => Some(query.expected_id()),
            _ => None,
        });
    let cap = budget.cap;
    let original_deadline = budget.deadline;
    let probe_cap = budget.spent + budget.remaining() / 2;
    let now = Instant::now();
    let probe_deadline = now
        .checked_add(original_deadline.saturating_duration_since(now) / 2)
        .unwrap_or(original_deadline)
        .min(original_deadline);
    let mut headers = Vec::new();
    for candidate in candidates {
        if visited.contains(&candidate) {
            continue;
        }
        if budget.spent >= probe_cap || Instant::now() >= probe_deadline {
            incomplete = true;
            enumeration_incomplete = true;
            break;
        }
        budget.cap = probe_cap;
        budget.deadline = probe_deadline;
        let result =
            codex_candidate_header(roots, &candidate, cwd, budget, CodexHeaderMode::ChildScan);
        budget.cap = cap;
        budget.deadline = original_deadline;
        match result {
            Ok((_, _, header))
                if header.id != query.expected_id()
                    && root.is_none_or(|root| root == header.root) =>
            {
                headers.push((candidate, header));
            }
            Ok(_) => {}
            Err(error) if error.is::<CodexForeignScope>() || error.is::<CodexNotChild>() => {}
            Err(_) => {
                incomplete = true;
                enumeration_incomplete = true;
            }
        }
    }
    let mut selected = headers
        .iter()
        .enumerate()
        .filter_map(|(index, (_, header))| {
            match codex_header_membership(header, &headers, query.expected_id()) {
                CodexHeaderMembership::Descendant(depth) => Some((index, depth)),
                CodexHeaderMembership::Outside => None,
                CodexHeaderMembership::Unknown => {
                    enumeration_incomplete = true;
                    (header.root == query.expected_id())
                        .then_some((index, MAX_SUBAGENT_SOURCES_PER_CAPTURE + 1))
                }
            }
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|(index, depth)| (*depth, *index));
    let mut evidence = HashMap::new();
    if let Some(parent) = parent.as_ref() {
        evidence.insert(
            (parent.header.root.clone(), parent.header.id.clone()),
            parent.clone(),
        );
    }
    let mut sources: Vec<DiscoveredSubagentContent> = Vec::new();
    let mut conflicts = std::collections::HashSet::new();
    let limit = source_limit.min(MAX_SUBAGENT_SOURCES_PER_CAPTURE);
    for (index, _) in selected {
        if budget.remaining() == 0 || Instant::now() >= budget.deadline {
            incomplete = true;
            enumeration_incomplete = true;
            break;
        }
        let (candidate, header) = &headers[index];
        let Some(parent_id) = header.parent.as_ref() else {
            continue;
        };
        let key = codex_child_logical_source_key(&header.root, parent_id, &header.id);
        if sources.len() >= limit && !sources.iter().any(|source| source.source_key == key) {
            incomplete = true;
            enumeration_incomplete = true;
            continue;
        }
        let parent_evidence = evidence.get(&(header.root.clone(), parent_id.clone()));
        match read_codex_discovered_child_with_header(
            roots,
            candidate,
            cwd,
            query,
            parent_evidence,
            budget,
            Some(header),
        ) {
            Ok(Some(source)) => {
                if let Some(old) = sources
                    .iter_mut()
                    .find(|old| old.source_key == source.source_key)
                {
                    if source.bytes.starts_with(&old.bytes) {
                        *old = source;
                        if conflicts.contains(&old.source_key) {
                            old.partial = true;
                        }
                    } else if !old.bytes.starts_with(&source.bytes) {
                        conflicts.insert(old.source_key.clone());
                        old.partial = true;
                        incomplete = true;
                        enumeration_incomplete = true;
                    }
                    if old.partial
                        && let Some(binding) = old.codex_binding.as_mut()
                    {
                        binding.hook_turn = None;
                    }
                } else {
                    sources.push(source);
                }
                if let Some(source) = sources.iter().find(|source| source.source_key == key) {
                    let next = match codex_parent_evidence_from_bytes(
                        header.clone(),
                        &source.bytes,
                        source.partial,
                        budget.deadline,
                    ) {
                        Ok(next) => next,
                        Err(_) => {
                            incomplete = true;
                            enumeration_incomplete = true;
                            CodexParentDiscoveryEvidence {
                                header: header.clone(),
                                spawned: Default::default(),
                                partial: true,
                            }
                        }
                    };
                    enumeration_incomplete |= next.partial;
                    let identity = (header.root.clone(), header.id.clone());
                    if let Some(old) = evidence.get(&identity)
                        && old.header.parent != next.header.parent
                    {
                        enumeration_incomplete = true;
                    }
                    evidence.insert(identity, next);
                }
            }
            Ok(None) => {
                enumeration_incomplete = true;
                evidence.insert(
                    (header.root.clone(), header.id.clone()),
                    CodexParentDiscoveryEvidence {
                        header: header.clone(),
                        spawned: Default::default(),
                        partial: true,
                    },
                );
            }
            Err(_) => {
                incomplete = true;
                enumeration_incomplete = true;
            }
        }
    }
    for ((root, id), parent) in &evidence {
        let missing = parent.spawned.iter().any(|child| {
            !sources.iter().any(|source| {
                source.codex_binding.as_ref().is_some_and(|binding| {
                    binding.root == *root && binding.parent == *id && binding.child == *child
                })
            })
        });
        enumeration_incomplete |= parent.partial || missing;
        if missing
            && !matches!(
                query.selection,
                CodexChildDiscoverySelection::Recovery { .. }
            )
        {
            incomplete = true;
        }
    }
    incomplete |= sources.iter().any(|source| source.partial);
    enumeration_incomplete |= incomplete || !codex_discovery_graph_closed(&sources, query);
    Ok(SubagentDiscovery {
        sources,bytes_read:budget.spent,incomplete,
        warning:incomplete.then(||"bounded Codex child discovery is incomplete; retry from the original authorized environment".into()),
        availability:super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available,
        usage_enumeration:Some(SubagentUsageEnumeration::from_query(query,!enumeration_incomplete)),
    })
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

fn update_subagent_source_preimage(digest: &mut Sha256, value: &[u8]) -> Result<()> {
    let length =
        u64::try_from(value.len()).context("subagent source identity field exceeds u64")?;
    digest.update(length.to_be_bytes());
    digest.update(value);
    Ok(())
}

/// Build a fixed, transient proof from helper-authorized source facts. It is
/// deliberately not a durable key: only the scoped repository HMAC below may
/// enter claims, revisions, checkpoint metadata, or traces.
fn subagent_source_preimage(
    provider_kind: &str,
    source_key: &str,
    capture_incarnation: Option<&str>,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-source-preimage-v2\0");
    update_subagent_source_preimage(&mut digest, provider_kind.as_bytes())?;
    update_subagent_source_preimage(&mut digest, source_key.as_bytes())?;
    match capture_incarnation {
        Some(incarnation) => {
            digest.update([1]);
            update_subagent_source_preimage(&mut digest, incarnation.as_bytes())?;
        }
        None => digest.update([0]),
    }
    Ok(digest.finalize().into())
}

fn is_subagent_source_commitment_v2(value: &str) -> bool {
    let Some(value) = value.strip_prefix("source/subagent-hmac-v2/") else {
        return false;
    };
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// V1 checkpoint metadata/traces are immutable historical proof. A matching
/// V1 claim is therefore not silently rewritten or shadowed with another V2
/// source row: recovery must remain on its legacy path until an explicit,
/// future migration can prove all immutable references consistently.
async fn reject_matching_legacy_subagent_claim(
    conn: &DatabaseConnection,
    parent_session_id: &str,
    provider_kind: &str,
    legacy_source_key: &str,
    deadline: Option<Instant>,
) -> Result<()> {
    let legacy = await_subagent_precommit_read_until(deadline, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?",
            [
                parent_session_id.into(),
                provider_kind.into(),
                legacy_source_key.into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION_V1.into(),
            ],
        ))
        .await
        .context("check legacy subagent content proof before V2 materialization")
    })
    .await?;
    if legacy.is_some() {
        bail!(
            "subagent content has a legacy immutable source proof; inspect it with `libra agent doctor` before capturing this source again"
        );
    }
    Ok(())
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
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cwd, deadline, byte_budget, source_limit);
        return Ok(SubagentDiscovery {
            warning: Some(
                "secure subagent content discovery is unavailable on this platform".to_string(),
            ),
            ..SubagentDiscovery::default()
        });
    }
    #[cfg(any(unix, windows))]
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
        let mut names = Vec::new();
        let mut entry_count = 0usize;
        for entry in super::observed_agents::read_dir_pinned_provider_directory(&directory)
            .context("enumerate pinned Claude subagent directory")?
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
            let file_type = entry.file_type;
            if file_type.is_symlink() {
                bail!("refusing symlink in Claude subagent directory (fail-closed)");
            }
            let name = entry.file_name;
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
            let bytes = match read_strictly_bounded(&mut file, remaining) {
                StrictBoundedRead::Complete(bytes) => bytes,
                StrictBoundedRead::Oversize { .. } => {
                    bail!(
                        "Claude subagent transcript set grew beyond {effective_budget} byte input budget while reading"
                    );
                }
                StrictBoundedRead::Failed { error, .. } => {
                    return Err(error).context("read securely opened Claude subagent source");
                }
            };
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
                legacy_source_key: None,
                bytes,
                malformed_lines,
                partial,
                // Claude's disk filename is not a provider guarantee that also
                // appears on hook boundaries, so guessing from it is forbidden.
                stable_subagent_id: None,
                codex_binding: None,
            });
        }
        Ok(SubagentDiscovery {
            sources: discovered,
            bytes_read: total_bytes,
            warning: None,
            incomplete: false,
            ..SubagentDiscovery::default()
        })
    })();
    #[cfg(any(unix, windows))]
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationOutcome {
    Reserved {
        fence_token: i64,
        revision: i64,
    },
    Unchanged {
        checkpoint_exists: bool,
        revision: i64,
    },
    Inflight {
        lease_expires_at: i64,
    },
    DurabilityProofStale,
}

struct ReservationAttempt<'a> {
    content_digest: &'a str,
    checkpoint_id: &'a str,
    owner: &'a str,
}

struct SourceReservationAttempt<'a> {
    attempt: ReservationAttempt<'a>,
    preparation: Option<&'a ChildPreparationFence>,
}

struct PreparedSourceReservation {
    outcome: ReservationOutcome,
    requirement: Option<ChildCheckpointRequirement>,
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

/// Acquire the SQLite writer slot while the monotonic half of a normal
/// child-content invocation is still live. The paired SQLite half is checked
/// later by the final authorization immediately before the non-cancellable
/// COMMIT.
async fn begin_subagent_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?
    .with_context(|| format!("{operation}: acquire subagent database writer"))
}

/// Bound only pure SQLite reads that precede a subagent-content mutation.
///
/// A dispatched DML statement, final authorization, and COMMIT are
/// intentionally excluded: cancelling any of those futures would leave the
/// durable outcome unknowable. Callers retain their transaction and roll it
/// back explicitly when this read-side deadline expires.
async fn await_subagent_precommit_read_until<T>(
    deadline: Option<Instant>,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_before_deadline(deadline)?;
    let value = match deadline {
        Some(deadline) => tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), read)
            .await
            .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?,
        None => read.await,
    }?;
    ensure_before_deadline(deadline)?;
    Ok(value)
}

/// Cleanup may only release a reservation that this invocation already owns.
/// Its paired recovery deadline bounds writer acquisition and the final SQLite
/// authorization. COMMIT remains non-cancellable once that authorization has
/// accepted the release.
async fn begin_subagent_cleanup_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?
    .with_context(|| format!("{operation}: acquire subagent cleanup database writer"))
}

/// Make final SQLite authorization the last SQL statement in a standalone
/// child-content transaction, then await COMMIT without cancellation. SQLx
/// may dispatch COMMIT before a cancelled acknowledgement future is dropped,
/// so wrapping COMMIT in `timeout_at` cannot prove an expired capture left no
/// new claim, link, or marker.
async fn commit_subagent_transaction_until(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    if let Some(deadline) = deadline
        && let Err(error) = ensure_before_deadline(Some(deadline.monotonic()))
    {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }
    let authorization = authorize_final_capture_commit(Some(scope), &txn, deadline).await;
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(match error {
            CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                anyhow::Error::from(SubagentCaptureDeadline)
            }
            error @ (CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
            | CaptureFinalCommitAuthorizationError::Database(_)) => anyhow::Error::new(error),
        })
        .with_context(|| format!("{operation}: authorize final subagent database commit"));
    }
    txn.commit().await.with_context(|| operation)
}

/// Reservation probes normally perform no durable write. If they had to clear
/// an expired reservation while proving an unchanged leaf, that narrow change
/// is cleanup of an existing provisional row and must settle within its paired
/// recovery deadline; otherwise roll the read-only transaction back.
async fn finish_subagent_reservation_probe(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    cleanup_deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    if let Some(cleanup_deadline) = cleanup_deadline {
        commit_subagent_transaction_until(txn, scope, Some(cleanup_deadline), operation).await
    } else {
        txn.rollback().await.with_context(|| operation)
    }
}

#[cfg(test)]
async fn reserve_source(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    attempt: ReservationAttempt<'_>,
    durability_proof: &UnchangedDurabilityProof,
    deadline: CaptureCommitDeadline,
) -> Result<ReservationOutcome> {
    reserve_source_prepared(
        conn,
        scope,
        parent_session_id,
        source,
        SourceReservationAttempt {
            attempt,
            preparation: None,
        },
        durability_proof,
        deadline,
    )
    .await
    .map(|result| result.outcome)
}

async fn reserve_source_prepared(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    attempt: SourceReservationAttempt<'_>,
    durability_proof: &UnchangedDurabilityProof,
    deadline: CaptureCommitDeadline,
) -> Result<PreparedSourceReservation> {
    // This is the first durable child-content sink. Keep the invariant here,
    // rather than relying only on the discovery/capture caller, because test
    // and recovery seams can call reservation directly.
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent content reservation requires a V2 source commitment");
    }
    let SourceReservationAttempt {
        attempt,
        preparation,
    } = attempt;
    let ReservationAttempt {
        content_digest,
        checkpoint_id,
        owner,
    } = attempt;
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let now_ms = Utc::now().timestamp_millis();
    let txn = begin_subagent_write_transaction_until(
        conn,
        deadline,
        "begin subagent content reservation",
    )
    .await?;
    if let Err(error) = await_subagent_precommit_read_until(
        Some(deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before reserving subagent content");
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let mut requirement = match preparation {
        Some(fence) => match async {
            fence
                .enroll(
                    &txn,
                    ChildCheckpointRequirement::new(&source.source_key, 1)?,
                    deadline,
                )
                .await
        }
        .await
        {
            Ok(requirement) => Some(requirement),
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error.into());
            }
        },
        None => None,
    };
    if let Err(error) = ensure_before_deadline(Some(deadline.monotonic())) {
        txn.rollback().await.ok();
        return Err(error);
    }
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
        commit_subagent_transaction_until(
            txn,
            scope,
            Some(deadline),
            "commit initial subagent content reservation",
        )
        .await?;
        return Ok(PreparedSourceReservation {
            outcome: ReservationOutcome::Reserved {
                fence_token: 1,
                revision: 1,
            },
            requirement,
        });
    }

    let mut cleanup_deadline = None;
    let row = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
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
        .context("read existing subagent content claim")
    })
    .await
    {
        Ok(row) => row,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    }
    .context("subagent content claim disappeared during reservation")?;
    let revision_cursor: i64 = row.try_get_by("revision_cursor")?;
    let current_revision: i64 = row.try_get_by("current_revision")?;
    let current_checkpoint_id: Option<String> = row.try_get_by("current_checkpoint_id")?;
    let current_digest: Option<String> = row.try_get_by("current_digest")?;
    let state: String = row.try_get_by("state")?;
    let attempt_digest: Option<String> = row.try_get_by("attempt_digest")?;
    let current_lease: Option<i64> = row.try_get_by("lease_expires_at")?;
    let fence_token: i64 = row.try_get_by("fence_token")?;
    if let Some(fence) = preparation {
        let enrolled = async {
            let floor = if state == "reserved" || current_digest.as_deref() != Some(content_digest)
            {
                revision_cursor
                    .checked_add(1)
                    .ok_or(CaptureCatalogError::InvalidRequest)?
            } else {
                current_revision
            };
            fence
                .enroll(
                    &txn,
                    ChildCheckpointRequirement::new(&source.source_key, floor)?,
                    deadline,
                )
                .await
        }
        .await;
        requirement = Some(match enrolled {
            Ok(requirement) => requirement,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error.into());
            }
        });
    }
    if let Err(error) = ensure_before_deadline(Some(deadline.monotonic())) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if state == "reserved" && current_lease.is_some_and(|lease| lease > now_ms) {
        let lease_expires_at = current_lease.unwrap_or(now_ms);
        if preparation.is_some() {
            commit_subagent_transaction_until(
                txn,
                scope,
                Some(deadline),
                "preserve in-flight child requirement",
            )
            .await?;
        } else {
            txn.rollback()
                .await
                .context("roll back in-flight subagent content probe")?;
        }
        tracing::debug!(
            same_digest = attempt_digest.as_deref() == Some(content_digest),
            "subagent content source is owned by another live writer"
        );
        return Ok(PreparedSourceReservation {
            outcome: ReservationOutcome::Inflight { lease_expires_at },
            requirement,
        });
    }
    if preparation.is_none()
        && state == "reserved"
        && current_digest.as_deref() == Some(content_digest)
    {
        let deadline_for_cleanup = subagent_cleanup_deadline(deadline)?;
        ensure_before_deadline(Some(deadline_for_cleanup.monotonic()))?;
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
            return Ok(PreparedSourceReservation {
                outcome: ReservationOutcome::Inflight {
                    lease_expires_at: now_ms.saturating_add(25),
                },
                requirement,
            });
        }
        cleanup_deadline = Some(deadline_for_cleanup);
    }
    if current_digest.as_deref() == Some(content_digest)
        && requirement
            .as_ref()
            .is_none_or(|required| required.minimum_revision() <= current_revision)
    {
        let probe_deadline = cleanup_deadline.unwrap_or(deadline);
        let current_checkpoint_id = current_checkpoint_id.as_deref().context(
            "subagent content claim has a digest but no current checkpoint; run `libra agent doctor`",
        )?;
        let intact =
            match await_subagent_precommit_read_until(Some(probe_deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
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
                .context("verify unchanged subagent content relation")
            })
            .await
            {
                Ok(intact) => intact,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            };
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
            finish_subagent_reservation_probe(
                txn,
                scope,
                cleanup_deadline.or(preparation.map(|_| deadline)),
                "finish stale subagent durability proof probe",
            )
            .await?;
            return Ok(PreparedSourceReservation {
                outcome: ReservationOutcome::DurabilityProofStale,
                requirement,
            });
        }
        let current_head =
            match await_subagent_precommit_read_until(Some(probe_deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .context("recheck traces head for unchanged subagent content")
            })
            .await
            {
                Ok(current_head) => current_head,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            }
            .map(|row| row.try_get_by::<Option<String>, _>("commit"))
            .transpose()?
            .flatten();
        if current_head != durability_proof.traces_head {
            finish_subagent_reservation_probe(
                txn,
                scope,
                cleanup_deadline.or(preparation.map(|_| deadline)),
                "finish changed traces-head durability probe",
            )
            .await?;
            return Ok(PreparedSourceReservation {
                outcome: ReservationOutcome::DurabilityProofStale,
                requirement,
            });
        }
        finish_subagent_reservation_probe(
            txn,
            scope,
            cleanup_deadline.or(preparation.map(|_| deadline)),
            "finish unchanged subagent content probe",
        )
        .await?;
        return Ok(PreparedSourceReservation {
            outcome: ReservationOutcome::Unchanged {
                checkpoint_exists: true,
                revision: current_revision,
            },
            requirement,
        });
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
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
        if preparation.is_some() {
            commit_subagent_transaction_until(
                txn,
                scope,
                Some(deadline),
                "preserve contended child requirement",
            )
            .await?;
        } else {
            txn.rollback()
                .await
                .context("roll back lost subagent content reservation")?;
        }
        return Ok(PreparedSourceReservation {
            outcome: ReservationOutcome::Inflight {
                lease_expires_at: now_ms.saturating_add(25),
            },
            requirement,
        });
    }
    let fence_row = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
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
        .context("read subagent content reservation fence")
    })
    .await
    {
        Ok(fence_row) => fence_row,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    }
    .context("subagent content claim disappeared after takeover")?;
    let fence_token: i64 = fence_row.try_get_by("fence_token")?;
    commit_subagent_transaction_until(
        txn,
        scope,
        Some(deadline),
        "commit subagent content reservation",
    )
    .await?;
    Ok(PreparedSourceReservation {
        outcome: ReservationOutcome::Reserved {
            fence_token,
            revision: revision_cursor.saturating_add(1),
        },
        requirement,
    })
}

struct ReservationRelease<'a> {
    preparation: Option<&'a ChildPreparationFence>,
    parent_session_id: &'a str,
    source: &'a DiscoveredSubagentContent,
    checkpoint_id: &'a str,
    owner: &'a str,
    fence_token: i64,
}

async fn release_reservation(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    release: ReservationRelease<'_>,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let ReservationRelease {
        preparation,
        parent_session_id,
        source,
        checkpoint_id,
        owner,
        fence_token,
    } = release;
    let cleanup_deadline = subagent_cleanup_deadline(deadline)?;
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent content reservation release requires a V2 source commitment");
    }
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    subagent_content_test_failpoint("before_release_reservation")?;
    let txn = begin_subagent_cleanup_transaction_until(
        conn,
        cleanup_deadline,
        "begin subagent content reservation release",
    )
    .await?;
    if let Some(fence) = preparation
        && let Err(error) = fence.assert_current(&txn).await
    {
        txn.rollback().await.ok();
        return Err(error.into());
    }
    if let Err(error) = await_subagent_precommit_read_until(
        Some(cleanup_deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before releasing subagent content reservation",
        );
    }
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    let released = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
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
    // Cleanup has a distinct short recovery grace, but it must still stop
    // before final authorization if that grace itself is exhausted.
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    commit_subagent_transaction_until(
        txn,
        scope,
        Some(cleanup_deadline),
        "commit subagent content reservation release",
    )
    .await?;
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

/// Markers are mutable recovery evidence. The traces API validates the scope
/// inside its own registration transaction, avoiding a check-to-write race.
async fn register_traces_write_attempt_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    marker: &TracesInflightMarker,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    // The traces API bounds its preparation and performs the final SQLite
    // authorization itself. Do not wrap this call in another timeout: that
    // could cancel an acknowledgement after its COMMIT was dispatched.
    match crate::internal::ai::traces::register_traces_write_attempt_with_capture_scope_until(
        conn,
        Some(scope),
        marker,
        &[],
        Some(CheckpointScope::Subagent),
        None,
        deadline,
    )
    .await
    .map_err(normalize_subagent_marker_deadline)
    .context("register subagent content traces write attempt")?
    {
        crate::internal::ai::traces::TracesWriteAttemptRegistration::Registered => Ok(()),
        crate::internal::ai::traces::TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration => {
            bail!("subagent content traces writer attempt is already in flight")
        }
        crate::internal::ai::traces::TracesWriteAttemptRegistration::AlreadyCommitted => {
            bail!("subagent content checkpoint is already durably committed")
        }
        crate::internal::ai::traces::TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete => {
            bail!("subagent content marker registration received an invalid terminal receipt acknowledgement")
        }
    }
}

/// A stale owner must also leave a marker alone: the traces API validates the
/// scope inside its cleanup transaction, preserving doctor/GC evidence when
/// the lease has expired.
async fn clear_non_cleanup_traces_inflight_marker_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    checkpoint_id: &str,
    marker_generation: &str,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    // The traces API owns both the final authorization and the unbounded
    // COMMIT acknowledgement for this ordinary (non-cleanup) mutation.
    crate::internal::ai::traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
        conn,
        Some(scope),
        parent_session_id,
        checkpoint_id,
        marker_generation,
        deadline,
    )
    .await
    .map_err(normalize_subagent_marker_deadline)
    .context("clear subagent content in-flight marker")?;
    Ok(())
}

async fn unique_boundary_checkpoint<C: ConnectionTrait>(
    conn: &C,
    parent_session_id: &str,
    stable_subagent_id: Option<&str>,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    let Some(stable_id) = stable_subagent_id else {
        return Ok(None);
    };
    let rows = await_subagent_precommit_read_until(deadline, async {
        conn.query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint
             WHERE session_id = ? AND scope = 'subagent'
               AND (subagent_session_id = ? OR tool_use_id = ?)
             ORDER BY created_at, checkpoint_id LIMIT 2",
            [parent_session_id.into(), stable_id.into(), stable_id.into()],
        ))
        .await
        .context("resolve stable subagent boundary association")
    })
    .await?;
    if rows.len() != 1 {
        return Ok(None);
    }
    rows.into_iter()
        .next()
        .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
        .transpose()
        .context("read unique subagent boundary checkpoint")
}

#[cfg(test)]
async fn refresh_current_link(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    refresh_current_link_prepared(conn, scope, parent_session_id, source, deadline, None).await
}

async fn refresh_current_link_prepared(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    deadline: CaptureCommitDeadline,
    preparation: Option<&ChildPreparationFence>,
) -> Result<()> {
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent link refresh requires a V2 source commitment");
    }
    if source.codex_binding.is_some() {
        // A fresh observed turn and exact revision proof are required for
        // these sources. Import/replay cannot infer an earlier boundary.
        return Ok(());
    }
    let Some(stable_id) = source.stable_subagent_id.as_deref() else {
        return Ok(());
    };
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let txn = begin_subagent_write_transaction_until(conn, deadline, "begin subagent link refresh")
        .await?;
    if let Err(error) = await_subagent_precommit_read_until(
        Some(deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before refreshing subagent link");
    }
    if let Some(fence) = preparation
        && let Err(error) = fence.assert_current(&txn).await
    {
        txn.rollback().await.ok();
        return Err(error.into());
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let current = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
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
        .context("read current subagent checkpoint for link refresh")
    })
    .await
    {
        Ok(current) => current,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let Some(current) = current else {
        txn.rollback()
            .await
            .context("roll back empty subagent link refresh")?;
        return Ok(());
    };
    let checkpoint_id: Option<String> = current.try_get_by("current_checkpoint_id")?;
    let Some(checkpoint_id) = checkpoint_id else {
        txn.rollback()
            .await
            .context("roll back unmaterialized subagent link refresh")?;
        return Ok(());
    };
    let boundary = match unique_boundary_checkpoint(
        &txn,
        parent_session_id,
        Some(stable_id),
        Some(deadline.monotonic()),
    )
    .await
    {
        Ok(boundary) => boundary,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let Some(boundary) = boundary else {
        txn.rollback()
            .await
            .context("roll back unresolved subagent link refresh")?;
        return Ok(());
    };
    ensure_before_deadline(Some(deadline.monotonic()))?;
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
    commit_subagent_transaction_until(txn, scope, Some(deadline), "commit subagent link refresh")
        .await?;
    Ok(())
}

struct ContentCommitPlan {
    preparation: Option<ChildPreparationFence>,
    capture_scope: CaptureScope,
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
    codex_binding: Option<CodexChildBinding>,
    created_at: i64,
    deadline: CaptureCommitDeadline,
}

impl fmt::Debug for ContentCommitPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentCommitPlan")
            .field("revision", &self.revision)
            .field("partial", &self.partial)
            .field("bound_child", &self.codex_binding.is_some())
            .finish()
    }
}

impl ContentCommitPlan {
    async fn apply_until(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // The transaction writes the claim, revision, checkpoint metadata,
        // and link. Reject a forged direct plan before any of those durable
        // surfaces can receive a provider locator or legacy SHA proof.
        if !is_subagent_source_commitment_v2(&self.source_key) {
            bail!("subagent content finalization requires a V2 source commitment");
        }
        if (self.provider_kind == "codex") != self.codex_binding.is_some() {
            bail!("subagent content finalization has inconsistent source binding");
        }
        if let Some(binding) = self.codex_binding.as_ref() {
            binding.validate()?;
            if binding.capture_session != self.parent_session_id
                || self.stable_subagent_id.as_deref() != Some(binding.child.as_str())
            {
                bail!("Codex child content identity changed before commit; retry the capture");
            }
        }
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        await_subagent_precommit_read_until(
            Some(self.deadline.monotonic()),
            self.capture_scope.assert_workspace_fence_live(txn),
        )
        .await
        .context("verify capture workspace lease before final subagent content transaction")?;
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        if let Some(fence) = &self.preparation {
            fence.assert_current(txn).await?;
        }
        subagent_content_test_failpoint("before_final_sql")?;
        let writable =
            await_subagent_precommit_read_until(Some(self.deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
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
                .context("verify subagent content tombstone write barrier")
            })
            .await?;
        if writable.is_none() {
            bail!("parent agent session was erased while subagent content was in flight");
        }

        let reservation =
            await_subagent_precommit_read_until(Some(self.deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
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
                .context("verify subagent content reservation fence")
            })
            .await?
            .context("subagent content reservation was lost before final commit")?;
        let revision_cursor: i64 = reservation.try_get_by("revision_cursor")?;
        if self.revision != revision_cursor.saturating_add(1) {
            bail!("subagent content revision changed while writer was in flight");
        }

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
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

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
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

        let boundary = if self.codex_binding.is_some() {
            None
        } else {
            unique_boundary_checkpoint(
                txn,
                &self.parent_session_id,
                self.stable_subagent_id.as_deref(),
                Some(self.deadline.monotonic()),
            )
            .await?
        };
        let link_state = if boundary.is_some() {
            "resolved"
        } else {
            "unresolved"
        };
        let link_timestamp_ms = Utc::now().timestamp_millis();
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
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

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
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
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl TracesTxnExtra for ContentCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // `HistoryManager` owns final authorization and the following
        // non-cancellable COMMIT. An outer timeout here could cancel that
        // acknowledgement after SQLite already accepted it.
        self.apply_until(txn, ctx).await
    }
}

async fn current_parent_commit(
    conn: &DatabaseConnection,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    await_subagent_precommit_read_until(deadline, async {
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
    })
    .await
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodexUsageState {
    Complete,
    Partial,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexUsageCounters {
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

fn codex_usage_candidate_depth(
    candidate: &CodexUsageCandidate,
    candidates: &[CodexUsageCandidate],
    current_thread: &str,
) -> Option<usize> {
    if candidate.child == current_thread || candidate.child == candidate.root {
        return None;
    }
    let mut parent = candidate.parent.as_str();
    let mut seen = std::collections::HashSet::from([candidate.child.as_str()]);
    for depth in 1..=MAX_SUBAGENT_SOURCES_PER_CAPTURE {
        if parent == current_thread {
            return Some(depth);
        }
        if !seen.insert(parent) {
            return None;
        }
        let mut ancestors = candidates
            .iter()
            .filter(|other| other.child == parent && other.root == candidate.root);
        let ancestor = ancestors.next()?;
        if ancestors.next().is_some() {
            return None;
        }
        parent = &ancestor.parent;
    }
    None
}

fn sum_codex_usage_counters(values: &[CodexUsageCounters]) -> Option<CodexUsageCounters> {
    fn optional_sum(a: Option<u64>, b: Option<u64>) -> Option<Option<u64>> {
        match (a, b) {
            (Some(a), Some(b)) => a.checked_add(b).map(Some),
            _ => Some(None),
        }
    }
    let mut sum = values.first()?.clone();
    for value in &values[1..] {
        sum.input_tokens = sum.input_tokens.checked_add(value.input_tokens)?;
        sum.output_tokens = sum.output_tokens.checked_add(value.output_tokens)?;
        sum.cached_tokens = optional_sum(sum.cached_tokens, value.cached_tokens)?;
        sum.reasoning_tokens = optional_sum(sum.reasoning_tokens, value.reasoning_tokens)?;
        sum.total_tokens = optional_sum(sum.total_tokens, value.total_tokens)?;
    }
    sum.valid().then_some(sum)
}

impl CodexUsageCounters {
    fn valid(&self) -> bool {
        self.input_tokens
            .checked_add(self.output_tokens)
            .is_some_and(|total| self.total_tokens.is_none_or(|reported| reported == total))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexThreadUsage {
    schema_version: u8,
    usage_state: CodexUsageState,
    token_usage: Option<CodexUsageCounters>,
}

impl Default for CodexThreadUsage {
    fn default() -> Self {
        Self {
            schema_version: 1,
            usage_state: CodexUsageState::Unknown,
            token_usage: None,
        }
    }
}

impl CodexThreadUsage {
    fn from_parent_extraction(value: &serde_json::Value, snapshot_complete: bool) -> Self {
        let usage = &value["token_usage"];
        let counters = serde_json::from_value::<CodexUsageCounters>(serde_json::json!({
            "input_tokens":usage["input_tokens"],"output_tokens":usage["output_tokens"],
            "cached_tokens":usage["cached_tokens"],"reasoning_tokens":usage["reasoning_tokens"],
            "total_tokens":usage["total_tokens"]
        }));
        let Ok(counters) = counters else {
            return Self::default();
        };
        let child_omission_only = value["warnings"].as_array().is_some_and(|warnings| {
            !warnings.is_empty()
                && warnings.iter().all(|warning| {
                    warning.as_str()
                        == Some("deadline-bound extraction omitted child source aggregation")
                })
        });
        Self {
            schema_version: 1,
            usage_state: if snapshot_complete
                && value["present"] == true
                && (value["partial"] == false || child_omission_only)
            {
                CodexUsageState::Complete
            } else {
                CodexUsageState::Partial
            },
            token_usage: Some(counters),
        }
        .after_metadata_redaction(&Redactor::new_default())
    }

    /// Apply the same small, closed metadata policy to observations from both
    /// newly written and immutable same-byte leaves. Never publish a counter
    /// that became another value (or invalid JSON) during credential scanning.
    fn after_metadata_redaction(self, redactor: &Redactor) -> Self {
        let Some(usage) = self.checked() else {
            return Self::default();
        };
        let Ok(expected) = serde_json::to_value(&usage) else {
            return Self::default();
        };
        let metadata = serde_json::json!({"subagent":{"usage":expected}});
        let Ok(bytes) = serde_json::to_vec_pretty(&metadata) else {
            return Self::default();
        };
        if bytes.len() > CODEX_CHILD_USAGE_CAP + 64 {
            return Self::default();
        }
        let (redacted, _) = redactor.redact(&bytes);
        let survived = serde_json::from_slice::<serde_json::Value>(redacted.as_ref())
            .ok()
            .is_some_and(|value| value.pointer("/subagent/usage") == Some(&expected));
        if survived { usage } else { Self::default() }
    }

    fn checked(self) -> Option<Self> {
        if self.schema_version != 1
            || (self.usage_state == CodexUsageState::Unknown) != self.token_usage.is_none()
            || self
                .token_usage
                .as_ref()
                .is_some_and(|usage| !usage.valid())
        {
            return None;
        }
        Some(self)
    }

    /// Usage is optional metadata over precisely the digested ordinary bytes.
    /// Its small cooperative slice never borrows a fresh source-read budget.
    fn from_redacted_snapshot(
        transcript: &RedactedBytes,
        thread: &str,
        source_partial: bool,
        original_newline_count: usize,
        not_after_millis: i64,
    ) -> Self {
        let Some(remaining) = not_after_millis.checked_sub(Utc::now().timestamp_millis()) else {
            return Self::default();
        };
        let Ok(budget_ms) = u64::try_from(remaining.min(40) / 4) else {
            return Self::default();
        };
        if budget_ms == 0 {
            return Self::default();
        }
        let Some(deadline) = Instant::now().checked_add(Duration::from_millis(budget_ms)) else {
            return Self::default();
        };
        // The ordinary credential policy can collapse a PEM span across JSONL
        // records. The remaining exact counters are useful, but cannot prove a
        // complete thread snapshot after an intervening record disappeared.
        // Conservatively mark any cross-record redaction as partial, even
        // when the removed span contained only ordinary message records.
        let mut retained_newlines = 0usize;
        for chunk in transcript.as_ref().chunks(16 * 1024) {
            if Instant::now() >= deadline {
                return Self::default();
            }
            retained_newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
            if retained_newlines >= original_newline_count {
                break;
            }
        }
        let records_removed = retained_newlines < original_newline_count;
        let summary = super::observed_agents::codex_rollout::extract_thread_usage_snapshot(
            transcript.as_ref(),
            thread,
            deadline,
        );
        if Instant::now() >= deadline {
            return Self::default();
        }
        let Some(usage) = summary.usage else {
            return Self::default();
        };
        Self {
            schema_version: 1,
            usage_state: if source_partial || records_removed || summary.partial {
                CodexUsageState::Partial
            } else {
                CodexUsageState::Complete
            },
            token_usage: Some(CodexUsageCounters {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                total_tokens: usage.total_tokens,
            }),
        }
        .checked()
        .unwrap_or_default()
    }
}

struct SafeSubagentProjection {
    transcript: RedactedBytes,
    report_value: serde_json::Value,
    partial: bool,
    turn_count: usize,
    content_digest: [u8; 32],
    codex_binding: Option<CodexChildBinding>,
    codex_usage: Option<CodexThreadUsage>,
}

fn redacted_subagent_metadata(
    metadata: &mut serde_json::Value,
    usage: &mut Option<CodexThreadUsage>,
) -> Result<RedactedBytes> {
    let encode = |metadata: &serde_json::Value| -> Result<RedactedBytes> {
        let bytes =
            serde_json::to_vec_pretty(metadata).context("serialize subagent content metadata")?;
        Ok(Redactor::new_default().redact(&bytes).0)
    };
    let redacted = encode(metadata)?;
    if let Some(expected) = usage.as_ref() {
        let expected =
            serde_json::to_value(expected).context("serialize ordinary child usage metadata")?;
        let survived = serde_json::from_slice::<serde_json::Value>(redacted.as_ref())
            .ok()
            .is_some_and(|value| value.pointer("/subagent/usage") == Some(&expected));
        if !survived {
            // A scrubbed number must never become a different accounting
            // value. Keep the ordinary content and an explicit unknown.
            let unknown = CodexThreadUsage::default();
            metadata["subagent"]["usage"] = serde_json::to_value(&unknown)
                .context("serialize unavailable child usage metadata")?;
            *usage = Some(unknown);
            return encode(metadata);
        }
    }
    Ok(redacted)
}

/// A serialization sink that refuses to grow the safe child projection beyond
/// the per-source cap. It writes directly into the final buffer so a single
/// pathological JSON turn cannot allocate an uncapped temporary before the
/// caller gets a chance to reject it.
struct BoundedSubagentProjectionWriter<'a> {
    bytes: &'a mut Vec<u8>,
    cap: u64,
}

impl std::io::Write for BoundedSubagentProjectionWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let next_len = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("safe subagent projection length overflow"))?;
        let next_len_u64 = u64::try_from(next_len)
            .map_err(|_| std::io::Error::other("safe subagent projection length overflow"))?;
        if next_len_u64 > self.cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "safe subagent projection exceeds its bounded output cap",
            ));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SafeSubagentProjection {
    #[cfg(test)]
    fn into_parts(self) -> (RedactedBytes, serde_json::Value, bool, usize) {
        (
            self.transcript,
            self.report_value,
            self.partial,
            self.turn_count,
        )
    }
}

impl fmt::Debug for SafeSubagentProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SafeSubagentProjection")
            .field("transcript_len", &self.transcript.len())
            .field("partial", &self.partial)
            .field("turn_count", &self.turn_count)
            .field("bound_child", &self.codex_binding.is_some())
            .finish()
    }
}

#[cfg(test)]
fn safe_content_projection(
    source: &DiscoveredSubagentContent,
) -> Result<(RedactedBytes, serde_json::Value, bool, usize)> {
    safe_content_projection_bounded(source, u64::MAX).map(SafeSubagentProjection::into_parts)
}

/// Normalize, redact, and serialize child content while retaining an explicit
/// byte ceiling for the projection. Production deadline callers run this
/// function only inside a killable helper; the synchronous wrapper remains
/// for non-deadlined unit fixtures and helper-side execution.
fn safe_content_projection_bounded(
    source: &DiscoveredSubagentContent,
    transcript_cap: u64,
) -> Result<SafeSubagentProjection> {
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
        let mut writer = BoundedSubagentProjectionWriter {
            bytes: &mut bytes,
            cap: transcript_cap,
        };
        serde_json::to_writer(&mut writer, &projection)
            .context("serialize safe subagent turn projection")?;
        std::io::Write::write_all(&mut writer, b"\n")
            .context("terminate safe subagent turn projection")?;
    }
    let transcript = RedactedBytes::new_unchecked(bytes);
    let content_digest =
        subagent_content_identity_digest_bytes(&transcript, partial, source.malformed_lines);
    Ok(SafeSubagentProjection {
        transcript,
        report_value: serde_json::to_value(report)
            .context("serialize subagent redaction report")?,
        partial,
        turn_count: turns.len(),
        content_digest,
        codex_binding: None,
        codex_usage: None,
    })
}

/// The request carries only the completeness metadata necessary to preserve
/// the durable content identity. Keeping it in the frame lets the helper hash
/// the final projection, so the deadline-owning parent never scans a large
/// transcript after its child exits.
fn encode_subagent_projection_request_header(
    source: &DiscoveredSubagentContent,
) -> Result<Vec<u8>> {
    encode_subagent_projection_work_request_header(source, None)
}

fn encode_subagent_projection_work_request_header(
    source: &DiscoveredSubagentContent,
    usage_not_after_millis: Option<i64>,
) -> Result<Vec<u8>> {
    let malformed_lines = u64::try_from(source.malformed_lines)
        .context("subagent projection malformed-line count overflow")?;
    let mut header = vec![0_u8; SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES];
    header[0] = u8::from(source.partial);
    header[1..].copy_from_slice(&malformed_lines.to_le_bytes());
    match (&source.codex_binding, source.provider_kind.as_str()) {
        (Some(binding), "codex") => {
            if source.stable_subagent_id.as_deref() != Some(binding.child.as_str()) {
                bail!("Codex child projection is missing its verified child identity");
            }
            let metadata = binding.encode()?;
            header[0] += 2;
            header.extend_from_slice(
                &u16::try_from(metadata.len())
                    .context("Codex child identity frame length overflow")?
                    .to_le_bytes(),
            );
            header.extend_from_slice(&metadata);
            if let Some(not_after) = usage_not_after_millis {
                header[0] += 2;
                header.extend_from_slice(&not_after.to_le_bytes());
            }
        }
        (None, "claude_code") => {}
        _ => bail!("subagent projection source has no matching provider identity"),
    }
    Ok(header)
}

#[cfg(all(test, feature = "test-live-agent"))]
fn decode_subagent_projection_request(input: Vec<u8>) -> Option<DiscoveredSubagentContent> {
    decode_subagent_projection_work_request(input).map(|(source, _)| source)
}

fn decode_subagent_projection_work_request(
    mut input: Vec<u8>,
) -> Option<(DiscoveredSubagentContent, Option<i64>)> {
    let input_len = u64::try_from(input.len()).ok()?;
    if input_len > SUBAGENT_PROJECTION_HELPER_INPUT_CAP
        || input.len() < SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES
    {
        return None;
    }
    let (partial, codex) = match input[0] {
        0 => (false, false),
        1 => (true, false),
        2 => (false, true),
        3 => (true, true),
        4 => (false, true),
        5 => (true, true),
        _ => return None,
    };
    let malformed_lines = <[u8; 8]>::try_from(&input[1..SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES])
        .ok()
        .map(u64::from_le_bytes)
        .and_then(|value| usize::try_from(value).ok())?;
    let (header_len, codex_binding) = if codex {
        let length_end = SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES.checked_add(2)?;
        let length = u16::from_le_bytes(
            input
                .get(SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES..length_end)?
                .try_into()
                .ok()?,
        ) as usize;
        let end = length_end.checked_add(length)?;
        let binding = CodexChildBinding::decode(input.get(length_end..end)?).ok()?;
        (end, Some(binding))
    } else {
        (SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES, None)
    };
    let (header_len, usage_not_after_millis) = if matches!(input[0], 4 | 5) {
        let end = header_len.checked_add(8)?;
        let not_after = i64::from_le_bytes(input.get(header_len..end)?.try_into().ok()?);
        (end, Some(not_after))
    } else {
        (header_len, None)
    };
    let byte_len = input.len().checked_sub(header_len)?;
    if u64::try_from(byte_len).ok()? > TRANSCRIPT_READ_HARD_CAP_BYTES {
        return None;
    }
    // Retain this exact bounded stdin allocation rather than cloning the raw
    // child transcript before normalization/redaction in the helper.
    input.copy_within(header_len.., 0);
    input.truncate(byte_len);
    Some((
        DiscoveredSubagentContent {
            provider_kind: if codex { "codex" } else { "claude_code" }.to_string(),
            source_key: String::new(),
            legacy_source_key: None,
            bytes: input,
            malformed_lines,
            partial,
            stable_subagent_id: codex_binding.as_ref().map(|binding| binding.child.clone()),
            codex_binding,
        },
        usage_not_after_millis,
    ))
}

/// Only small control fields are decoded here. Unknown content, including
/// reasoning bodies and ciphertext, stays in the ordinary snapshot byte stream.
struct CodexChildText(String);

impl<'de> Deserialize<'de> for CodexChildText {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct TextVisitor;
        impl serde::de::Visitor<'_> for TextVisitor {
            type Value = CodexChildText;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded Codex child metadata")
            }
            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                if value.len() > 128 {
                    return Err(E::custom("Codex child metadata exceeds its bound"));
                }
                Ok(CodexChildText(value.to_owned()))
            }
        }
        deserializer.deserialize_str(TextVisitor)
    }
}

#[derive(Deserialize)]
struct CodexChildRecord {
    #[serde(rename = "type")]
    kind: CodexChildText,
    payload: Option<CodexChildPayload>,
}

#[derive(Deserialize)]
struct CodexChildPayload {
    #[serde(rename = "type")]
    kind: Option<CodexChildText>,
    id: Option<CodexChildText>,
    session_id: Option<CodexChildText>,
    parent_thread_id: Option<CodexChildText>,
    thread_id: Option<CodexChildText>,
    turn_id: Option<CodexChildText>,
}

fn codex_child_text(value: &Option<CodexChildText>) -> Option<&str> {
    value.as_ref().map(|text| text.0.as_str())
}

struct CodexChildBodySummary {
    content_records: usize,
    // Count all raw line boundaries, including otherwise skipped blank lines.
    newline_count: usize,
    malformed_lines: usize,
    turn_count: usize,
    partial: bool,
    observed_turn: Option<String>,
}

fn summarize_codex_child_body(
    bytes: &[u8],
    binding: &CodexChildBinding,
) -> Result<CodexChildBodySummary> {
    const LINE_CAP: usize = 1024 * 1024;
    const TURN_CAP: usize = 4096;
    let mut header_seen = false;
    let mut summary = CodexChildBodySummary {
        content_records: 0,
        newline_count: 0,
        malformed_lines: 0,
        turn_count: 0,
        partial: false,
        observed_turn: None,
    };
    let mut turns = std::collections::BTreeSet::new();
    let mut last_turn = None;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        summary.newline_count += usize::from(line.ends_with(b"\n"));
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let complete_line = line.ends_with(b"\n");
        if !complete_line {
            // Preserve the tail as ordinary data, but it cannot prove where
            // the currently paused host turn is in a concurrently written file.
            summary.partial = true;
        }
        let record = if line.len() <= LINE_CAP {
            match serde_json::from_slice::<CodexChildRecord>(line) {
                Ok(record) => Some(record),
                Err(error) => {
                    if error.is_syntax() || error.is_eof() {
                        summary.malformed_lines = summary.malformed_lines.saturating_add(1);
                    }
                    None
                }
            }
        } else {
            None
        };
        let Some(record) = record else {
            if !header_seen {
                bail!("Codex child transcript has no valid bounded session header");
            }
            summary.partial = true;
            // Unknown or truncated content still goes through the ordinary
            // credential scanner. Lack of a recognized turn is not a reason
            // to discard authorized bytes or pretend their origin is native.
            summary.content_records = summary.content_records.saturating_add(1);
            continue;
        };
        let Some(payload) = record.payload else {
            if !header_seen {
                bail!("Codex child transcript has no valid session metadata");
            }
            summary.partial = true;
            summary.content_records = summary.content_records.saturating_add(1);
            continue;
        };
        if !header_seen {
            if record.kind.0 != "session_meta"
                || codex_child_text(&payload.id) != Some(binding.child.as_str())
                || codex_child_text(&payload.session_id) != Some(binding.root.as_str())
                || codex_child_text(&payload.parent_thread_id) != Some(binding.parent.as_str())
            {
                bail!("Codex child transcript does not match its verified source identity");
            }
            header_seen = true;
            continue;
        }
        match record.kind.0.as_str() {
            "session_meta" => summary.partial = true,
            "turn_context" => {
                last_turn = codex_child_text(&payload.turn_id)
                    .filter(|turn| codex_child_identifier(turn, 64))
                    .map(str::to_owned);
                if let Some(turn) = last_turn.as_ref() {
                    if turns.len() < TURN_CAP {
                        turns.insert(turn.clone());
                    } else if !turns.contains(turn) {
                        summary.partial = true;
                    }
                } else {
                    summary.partial = true;
                }
            }
            "response_item" => {
                if matches!(
                    codex_child_text(&payload.kind),
                    Some(
                        "reasoning"
                            | "message"
                            | "function_call"
                            | "function_call_output"
                            | "custom_tool_call"
                            | "custom_tool_call_output"
                    )
                ) {
                    summary.content_records = summary.content_records.saturating_add(1);
                } else {
                    summary.content_records = summary.content_records.saturating_add(1);
                    summary.partial = true;
                }
            }
            "event_msg" => match codex_child_text(&payload.kind) {
                Some("task_started" | "turn_started") => {
                    let started = codex_child_text(&payload.turn_id)
                        .filter(|turn| codex_child_identifier(turn, 64));
                    // Native startup persists this marker before turn_context.
                    // A new start invalidates the previous context; only a
                    // subsequent context may establish the current turn.
                    if started.is_none() || started != last_turn.as_deref() {
                        last_turn = None;
                    }
                    if started.is_none() {
                        summary.partial = true;
                    }
                }
                Some("agent_reasoning" | "agent_message" | "user_message") => {
                    summary.content_records = summary.content_records.saturating_add(1);
                }
                Some("task_complete" | "turn_complete" | "turn_aborted")
                    if codex_child_text(&payload.turn_id) != last_turn.as_deref() =>
                {
                    summary.partial = true;
                    last_turn = None;
                }
                _ => {}
            },
            // Codex 0.162.1 persists response usage before running Stop hooks.
            // It observes an existing turn; it cannot establish a new one.
            "token_usage_record" => {
                if codex_child_text(&payload.thread_id) != Some(binding.child.as_str())
                    || codex_child_text(&payload.session_id) != Some(binding.root.as_str())
                    || last_turn.is_none()
                    || codex_child_text(&payload.turn_id) != last_turn.as_deref()
                {
                    summary.partial = true;
                }
            }
            // This closed set contains snapshots without conversation turns.
            // Retain their original bytes and validate any explicit identity;
            // context-bearing/unknown kinds still take the partial path below.
            "world_state" | "security_risk_score" => {
                if codex_child_text(&payload.thread_id)
                    .is_some_and(|id| id != binding.child.as_str())
                    || codex_child_text(&payload.session_id)
                        .is_some_and(|id| id != binding.root.as_str())
                    || codex_child_text(&payload.turn_id)
                        .is_some_and(|turn| Some(turn) != last_turn.as_deref())
                {
                    summary.partial = true;
                }
            }
            _ => {
                summary.content_records = summary.content_records.saturating_add(1);
                summary.partial = true;
            }
        }
    }
    if !header_seen {
        bail!("Codex child transcript has no session header");
    }
    summary.turn_count = turns.len();
    // Normal core submissions use UUIDs, while internal context-only IDs use
    // a process-local counter. Those counters cannot bind a resumed hook.
    summary.observed_turn = last_turn.filter(|turn| {
        !summary.partial
            && binding.hook_turn.as_deref() == Some(turn.as_str())
            && uuid::Uuid::parse_str(turn).is_ok_and(|id| matches!(id.get_version_num(), 4 | 7))
    });
    Ok(summary)
}

/// Consume one helper-owned Codex input. The shared snapshot applies the
/// ordinary decoded credential scan without rebuilding away reasoning fields.
fn safe_codex_content_projection_with_usage_owned(
    mut source: DiscoveredSubagentContent,
    transcript_cap: u64,
    usage_not_after_millis: Option<i64>,
) -> Result<SafeSubagentProjection> {
    let mut binding = source
        .codex_binding
        .take()
        .context("Codex child projection requires a verified source binding")?;
    binding.validate()?;
    if source.provider_kind != "codex"
        || source.stable_subagent_id.as_deref() != Some(binding.child.as_str())
    {
        bail!("Codex child projection has inconsistent source identity");
    }
    let summary = summarize_codex_child_body(&source.bytes, &binding)?;
    if summary.content_records == 0 {
        bail!("Codex child transcript contains metadata only; no content checkpoint is available");
    }
    binding.hook_turn = summary.observed_turn;
    let auth = ExportAuthorized::issue_discovered_subagent(
        "codex",
        &binding.capture_session,
        &source.bytes,
    );
    let snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
        TranscriptSource::Bytes {
            bytes: source.bytes,
            auth,
        },
        super::observed_agents::agent_for(super::observed_agents::AgentKind::Codex),
        super::capture::snapshot::AuthorizedReasoningSourceContext::parent_only_child(
            "codex",
            &binding.capture_session,
        ),
        CaptureSnapshotPolicy::default(),
    );
    let report_value = serde_json::to_value(snapshot.redaction_report())
        .context("serialize ordinary Codex child redaction report")?;
    let partial = source.partial
        || source.malformed_lines > 0
        || summary.partial
        || snapshot.completeness()
            == super::capture::snapshot::CaptureSnapshotCompleteness::Partial;
    if partial {
        binding.hook_turn = None;
    }
    let transcript = snapshot
        .try_into_ordinary()
        .map_err(|_| anyhow!("Codex child snapshot has no ordinary content disposition"))?
        .filter(|transcript| !transcript.is_empty())
        .context("Codex child snapshot has no readable ordinary content")?;
    if transcript.len() as u64 > transcript_cap {
        bail!("Codex child projection exceeds its bounded output cap");
    }
    // Codex stores ordinary source bytes verbatim. Discovery-role certainty
    // can change between live/import/recovery without changing those bytes;
    // normalize that digest bit while retaining the actual partial metadata.
    // Claude's historical identity calculation remains unchanged.
    let content_digest =
        subagent_content_identity_digest_bytes(&transcript, false, source.malformed_lines);
    let codex_usage = usage_not_after_millis.map(|not_after| {
        CodexThreadUsage::from_redacted_snapshot(
            &transcript,
            &binding.child,
            partial,
            summary.newline_count,
            not_after,
        )
    });
    Ok(SafeSubagentProjection {
        transcript,
        report_value,
        partial,
        turn_count: summary.turn_count,
        content_digest,
        codex_binding: Some(binding),
        codex_usage,
    })
}

/// Private helper entry for CPU-heavy child content normalization, typed-field
/// redaction, and allowlist projection. Its input is exactly one bounded raw
/// child transcript on stdin plus bounded, owner-issued identity metadata.
/// Provider paths and raw errors never cross this projection protocol.
#[doc(hidden)]
pub fn run_subagent_projection_helper(input: Vec<u8>) -> Result<Vec<u8>> {
    let Some((source, usage_not_after_millis)) = decode_subagent_projection_work_request(input)
    else {
        return Ok(vec![SUBAGENT_PROJECTION_FAILED]);
    };
    let raw_bytes = u64::try_from(source.bytes.len()).ok();
    let response = raw_bytes.and_then(|raw_bytes| {
        (if source.provider_kind == "codex" {
            safe_codex_content_projection_with_usage_owned(
                source,
                subagent_projection_transcript_cap(raw_bytes),
                usage_not_after_millis,
            )
        } else {
            safe_content_projection_bounded(&source, subagent_projection_transcript_cap(raw_bytes))
        })
        .ok()
        .and_then(|projection| encode_subagent_projection_frame(projection, raw_bytes))
    });
    Ok(match response {
        Some(response) => response,
        None => vec![SUBAGENT_PROJECTION_FAILED],
    })
}

fn encode_subagent_projection_frame(
    projection: SafeSubagentProjection,
    raw_bytes: u64,
) -> Option<Vec<u8>> {
    let report = serde_json::to_vec(&projection.report_value).ok()?;
    let transcript_len = u64::try_from(projection.transcript.len()).ok()?;
    let report_len = u64::try_from(report.len()).ok()?;
    let binding = projection
        .codex_binding
        .as_ref()
        .map(CodexChildBinding::encode)
        .transpose()
        .ok()?;
    if binding.is_none() && projection.codex_usage.is_some() {
        return None;
    }
    let mut usage_section = binding.is_some() && projection.codex_usage.is_some();
    let mut usage = projection
        .codex_usage
        .as_ref()
        .and_then(|usage| usage.clone().checked())
        .and_then(|usage| serde_json::to_vec(&usage).ok())
        .filter(|bytes| bytes.len() <= CODEX_CHILD_USAGE_CAP);
    let binding_frame_len = binding.as_ref().map_or(0, |bytes| bytes.len() + 2);
    if usage_section
        && report_len
            .checked_add(binding_frame_len as u64)?
            .checked_add(2)?
            > SUBAGENT_PROJECTION_REPORT_CAP
    {
        // Even the optional length prefix must not displace a valid legacy
        // report. Its existing Codex layout carries the same ordinary bytes.
        usage_section = false;
        usage = None;
    }
    if usage_section
        && report_len
            .checked_add(binding_frame_len as u64)?
            .checked_add(2)?
            .checked_add(usage.as_ref().map_or(0, Vec::len) as u64)?
            > SUBAGENT_PROJECTION_REPORT_CAP
    {
        usage = None;
    }
    let usage_frame_len = if usage_section {
        2 + usage.as_ref().map_or(0, Vec::len)
    } else {
        0
    };
    let auxiliary_frame_len = binding_frame_len.checked_add(usage_frame_len)?;
    if transcript_len > subagent_projection_transcript_cap(raw_bytes)
        || report_len.checked_add(auxiliary_frame_len as u64)? > SUBAGENT_PROJECTION_REPORT_CAP
    {
        return None;
    }
    let turn_count = u64::try_from(projection.turn_count).ok()?;
    let report_len_u32 = u32::try_from(report_len).ok()?;
    let total_len = SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
        .checked_add(auxiliary_frame_len)?
        .checked_add(usize::try_from(transcript_len).ok()?)?
        .checked_add(usize::try_from(report_len).ok()?)?;
    if u64::try_from(total_len).ok()? > subagent_projection_response_cap(raw_bytes) {
        return None;
    }
    let mut frame = Vec::with_capacity(total_len);
    frame.push(if usage_section {
        SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE
    } else if binding.is_some() {
        SUBAGENT_PROJECTION_CODEX_COMPLETE
    } else {
        SUBAGENT_PROJECTION_COMPLETE
    });
    frame.push(u8::from(projection.partial));
    frame.extend_from_slice(&turn_count.to_le_bytes());
    frame.extend_from_slice(&transcript_len.to_le_bytes());
    frame.extend_from_slice(&report_len_u32.to_le_bytes());
    frame.extend_from_slice(&projection.content_digest);
    if let Some(binding) = binding {
        frame.extend_from_slice(&u16::try_from(binding.len()).ok()?.to_le_bytes());
        frame.extend_from_slice(&binding);
    }
    if usage_section {
        let usage = usage.as_deref().unwrap_or_default();
        frame.extend_from_slice(&u16::try_from(usage.len()).ok()?.to_le_bytes());
        frame.extend_from_slice(usage);
    }
    frame.extend_from_slice(projection.transcript.as_ref());
    frame.extend_from_slice(&report);
    Some(frame)
}

fn decode_subagent_projection_frame(
    mut frame: Vec<u8>,
    raw_bytes: u64,
) -> Result<SafeSubagentProjection> {
    if frame == [SUBAGENT_PROJECTION_FAILED] {
        bail!("killable subagent content projection helper reported a safe failure");
    }
    if frame.len() < SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
        || !matches!(
            frame.first().copied(),
            Some(
                SUBAGENT_PROJECTION_COMPLETE
                    | SUBAGENT_PROJECTION_CODEX_COMPLETE
                    | SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE
            )
        )
    {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let partial = match frame[1] {
        0 => false,
        1 => true,
        _ => bail!("killable subagent content projection helper returned an invalid response"),
    };
    let turn_count = <[u8; 8]>::try_from(&frame[2..10])
        .map(u64::from_le_bytes)
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .context("killable subagent content projection helper returned an invalid response")?;
    let transcript_len = <[u8; 8]>::try_from(&frame[10..18])
        .map(u64::from_le_bytes)
        .ok()
        .context("killable subagent content projection helper returned an invalid response")?;
    let report_len = <[u8; 4]>::try_from(&frame[18..22])
        .map(u32::from_le_bytes)
        .ok()
        .map(u64::from)
        .context("killable subagent content projection helper returned an invalid response")?;
    let (payload_start, codex_binding) = if matches!(
        frame[0],
        SUBAGENT_PROJECTION_CODEX_COMPLETE | SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE
    ) {
        let length_end = SUBAGENT_PROJECTION_FRAME_HEADER_BYTES + 2;
        let length = frame
            .get(SUBAGENT_PROJECTION_FRAME_HEADER_BYTES..length_end)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(u16::from_le_bytes)
            .map(usize::from)
            .context("Codex child projection returned an invalid identity frame")?;
        let end = length_end
            .checked_add(length)
            .context("Codex child projection identity frame length overflow")?;
        let binding = CodexChildBinding::decode(
            frame
                .get(length_end..end)
                .context("Codex child projection identity frame is truncated")?,
        )?;
        (end, Some(binding))
    } else {
        (SUBAGENT_PROJECTION_FRAME_HEADER_BYTES, None)
    };
    let (payload_start, mut codex_usage) = if frame[0] == SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE {
        let length_end = payload_start
            .checked_add(2)
            .context("Codex usage metadata frame length overflow")?;
        let length = frame
            .get(payload_start..length_end)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(u16::from_le_bytes)
            .map(usize::from)
            .context("Codex usage metadata frame is truncated")?;
        let end = length_end
            .checked_add(length)
            .context("Codex usage metadata frame length overflow")?;
        let bytes = frame
            .get(length_end..end)
            .context("Codex usage metadata frame is truncated")?;
        let usage = if bytes.len() <= CODEX_CHILD_USAGE_CAP {
            serde_json::from_slice::<CodexThreadUsage>(bytes)
                .ok()
                .and_then(CodexThreadUsage::checked)
                .unwrap_or_default()
        } else {
            CodexThreadUsage::default()
        };
        (end, Some(usage))
    } else {
        (payload_start, None)
    };
    if partial
        && let Some(usage) = codex_usage.as_mut()
        && usage.usage_state == CodexUsageState::Complete
    {
        usage.usage_state = CodexUsageState::Partial;
    }
    let binding_frame_len = payload_start - SUBAGENT_PROJECTION_FRAME_HEADER_BYTES;
    if transcript_len > subagent_projection_transcript_cap(raw_bytes)
        || report_len
            .checked_add(binding_frame_len as u64)
            .is_none_or(|length| length > SUBAGENT_PROJECTION_REPORT_CAP)
    {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let payload_len = usize::try_from(transcript_len)
        .ok()
        .and_then(|transcript_len| {
            usize::try_from(report_len)
                .ok()
                .and_then(|report_len| transcript_len.checked_add(report_len))
        })
        .context("killable subagent content projection helper returned an invalid response")?;
    let expected_len = payload_start
        .checked_add(payload_len)
        .context("killable subagent content projection helper returned an invalid response")?;
    let response_cap = subagent_projection_response_cap(raw_bytes);
    if frame.len() != expected_len || u64::try_from(frame.len()).ok() > Some(response_cap) {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let transcript_end =
        payload_start
            .checked_add(usize::try_from(transcript_len).context(
                "killable subagent content projection helper returned an invalid response",
            )?)
            .context("killable subagent content projection helper returned an invalid response")?;
    let report: RedactionReport = serde_json::from_slice(&frame[transcript_end..])
        .context("decode safe subagent projection redaction report")?;
    if report.matches.len() > MAX_REDACTION_MATCH_SAMPLES {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let content_digest =
        <[u8; 32]>::try_from(&frame[22..SUBAGENT_PROJECTION_FRAME_HEADER_BYTES])
            .context("killable subagent content projection helper returned an invalid response")?;
    // Keep the exact bounded stdout allocation for the redacted projection.
    // Cloning the payload here would briefly add another 1.5x raw buffer to
    // the hook parent and violate ACF-03's two-buffer memory budget.
    frame.copy_within(payload_start..transcript_end, 0);
    frame.truncate(
        usize::try_from(transcript_len)
            .context("killable subagent content projection helper returned an invalid response")?,
    );
    Ok(SafeSubagentProjection {
        transcript: RedactedBytes::new_unchecked(frame),
        report_value: serde_json::to_value(report)
            .context("serialize safe subagent projection redaction report")?,
        partial,
        turn_count,
        content_digest,
        codex_binding,
        codex_usage,
    })
}

/// Run the CPU-heavy projection under the same absolute deadline as child
/// persistence. The parent only copies bounded helper output; it never parses
/// or redacts native child transcript bytes in-process on a live deadline.
#[cfg(any(unix, windows))]
async fn safe_content_projection_until(
    source: &DiscoveredSubagentContent,
    deadline: Instant,
) -> Result<SafeSubagentProjection> {
    safe_content_projection_work_until(source, deadline, None).await
}

async fn safe_content_projection_with_usage_until(
    source: &DiscoveredSubagentContent,
    deadline: CaptureCommitDeadline,
    projection_deadline: Instant,
) -> Result<SafeSubagentProjection> {
    let work_deadline = projection_deadline.min(deadline.monotonic());
    let usage_not_after_millis = narrow_subagent_work_deadline(deadline, work_deadline)
        .map(CaptureCommitDeadline::sqlite_not_after_millis);
    safe_content_projection_work_until(source, work_deadline, usage_not_after_millis).await
}

/// Keep child persistence inside the original pair while reserving parent
/// time. The SQLite half is reduced with the monotonic half, never renewed.
pub(crate) fn import_child_work_deadline(
    original: CaptureCommitDeadline,
) -> Option<CaptureCommitDeadline> {
    let now = Instant::now();
    let remaining = original.monotonic().checked_duration_since(now)?;
    let reserve = Duration::from_secs(20).min(remaining / 2);
    let allowance = remaining.checked_sub(reserve)?.min(Duration::from_secs(8));
    if allowance.is_zero() {
        return None;
    }
    narrow_subagent_work_deadline(original, now.checked_add(allowance)?)
}

pub(crate) fn import_child_capture_deadline(
    work: CaptureCommitDeadline,
) -> Option<CaptureCommitDeadline> {
    let remaining = work.monotonic().checked_duration_since(Instant::now())?;
    let auxiliary = Duration::from_millis(100).min(remaining / 4);
    narrow_subagent_work_deadline(work, work.monotonic().checked_sub(auxiliary)?)
}

fn narrow_subagent_work_deadline(
    original: CaptureCommitDeadline,
    requested: Instant,
) -> Option<CaptureCommitDeadline> {
    let monotonic = requested.min(original.monotonic());
    let trim = original.monotonic().saturating_duration_since(monotonic);
    // Round the reduction up, so the SQLite half remains conservatively no
    // later than its paired work window. Neither clock is re-established.
    let trim_millis = i64::try_from(trim.as_nanos().div_ceil(1_000_000)).ok()?;
    let sqlite = original
        .sqlite_not_after_millis()
        .checked_sub(trim_millis)?;
    Some(CaptureCommitDeadline::from_established_pair(
        monotonic, sqlite,
    ))
}

#[cfg(any(unix, windows))]
async fn safe_content_projection_work_until(
    source: &DiscoveredSubagentContent,
    deadline: Instant,
    usage_not_after_millis: Option<i64>,
) -> Result<SafeSubagentProjection> {
    ensure_before_deadline(Some(deadline))?;
    let source_len = u64::try_from(source.bytes.len())
        .context("subagent content projection source length overflow")?;
    if source_len > TRANSCRIPT_READ_HARD_CAP_BYTES {
        bail!("subagent content projection source exceeds its bounded input cap");
    }
    let request_header = if usage_not_after_millis.is_some() {
        encode_subagent_projection_work_request_header(source, usage_not_after_millis)?
    } else {
        encode_subagent_projection_request_header(source)?
    };
    let Some(program) = projection_helper_program() else {
        #[cfg(test)]
        {
            // Unit tests link the library without the CLI main entrypoint.
            // Production must never take this synchronous path: it would
            // make a deadline-bound capture non-cancellable again.
            let projection = if source.provider_kind == "codex" {
                safe_codex_content_projection_with_usage_owned(
                    source.clone(),
                    subagent_projection_transcript_cap(source_len),
                    usage_not_after_millis,
                )?
            } else {
                safe_content_projection_bounded(
                    source,
                    subagent_projection_transcript_cap(source_len),
                )?
            };
            ensure_before_deadline(Some(deadline))?;
            return Ok(projection);
        }
        #[cfg(not(test))]
        bail!("killable subagent content projection helper is unavailable in this executable");
    };
    let mut command = tokio::process::Command::new(program);
    command
        .arg(SUBAGENT_PROJECTION_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_private_helper_process_group(&mut command);
    #[cfg(unix)]
    let child = command
        .spawn()
        .context("start killable subagent content projection helper")?;
    #[cfg(unix)]
    let mut child = CancellationSafeChild::new_process_group(child);
    #[cfg(windows)]
    let mut child = CancellationSafeChild::spawn_windows(&mut command)
        .context("start contained Windows subagent helper")?;
    let Some(mut stdin) = child.child_mut().and_then(|child| child.stdin.take()) else {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper has no stdin pipe");
    };
    let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper has no stdout pipe");
    };
    let output_cap = subagent_projection_response_cap(source_len);
    let mut output_task =
        tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
    child.register_abort_on_cancel(&output_task);
    let write_result: Result<()> =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            stdin.write_all(&request_header).await?;
            stdin.write_all(&source.bytes).await?;
            stdin.shutdown().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error).context("send killable subagent projection request"),
            Err(_) => Err(SubagentCaptureDeadline.into()),
        };
    if let Err(error) = write_result {
        child.terminate_and_reap();
        return Err(error);
    }
    // The projection helper consumes its complete request before producing a
    // response. `shutdown` flushes the async writer, but the pipe remains
    // owned until this handle is dropped; otherwise `child.wait()` can deadlock
    // behind a helper still waiting for EOF.
    drop(stdin);
    // Await stdout EOF before reaping the leader. A forked child can retain
    // both request/response pipes; the deadline path must still own the
    // leader PGID when it kills that descendant.
    let output =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut output_task)
            .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                bail!("killable subagent content projection helper returned an invalid response")
            }
            Err(_) => {
                output_task.abort();
                child.terminate_and_reap();
                return Err(SubagentCaptureDeadline.into());
            }
        };
    let output_len = u64::try_from(output.len())
        .context("killable subagent content projection helper output length overflow")?;
    if output_len > output_cap {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let status_result = match child.child_mut() {
        Some(child_process) => {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(error)) => {
                    Err(anyhow!(error).context("wait for killable subagent projection helper"))
                }
                Err(_) => Err(SubagentCaptureDeadline.into()),
            }
        }
        None => Err(anyhow!(
            "killable subagent content projection helper was unavailable"
        )),
    };
    let status = match status_result {
        Ok(status) => {
            child.disarm_child_after_wait();
            status
        }
        Err(error) => {
            child.terminate_and_reap();
            return Err(error);
        }
    };
    child.finish();
    if !status.success() {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    ensure_before_deadline(Some(deadline))?;
    let projection = decode_subagent_projection_frame(output, source_len)?;
    validate_subagent_projection_binding(source, &projection)?;
    ensure_before_deadline(Some(deadline))?;
    Ok(projection)
}

fn validate_subagent_projection_binding(
    source: &DiscoveredSubagentContent,
    projection: &SafeSubagentProjection,
) -> Result<()> {
    match (&source.codex_binding, &projection.codex_binding) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual))
            if expected.root == actual.root
                && expected.parent == actual.parent
                && expected.child == actual.child
                && expected.capture_session == actual.capture_session
                && (actual.hook_turn.is_none() || actual.hook_turn == expected.hook_turn) =>
        {
            Ok(())
        }
        _ => bail!("subagent projection helper returned a different source identity"),
    }
}

#[cfg(not(any(unix, windows)))]
async fn safe_content_projection_until(
    _source: &DiscoveredSubagentContent,
    _deadline: Instant,
) -> Result<SafeSubagentProjection> {
    bail!("killable subagent content projection helper is unavailable on this platform")
}

#[cfg(not(any(unix, windows)))]
async fn safe_content_projection_work_until(
    _source: &DiscoveredSubagentContent,
    _deadline: Instant,
    _usage_not_after_millis: Option<i64>,
) -> Result<SafeSubagentProjection> {
    bail!("killable subagent content projection helper is unavailable on this platform")
}

fn subagent_content_identity_digest_bytes(
    transcript: &RedactedBytes,
    partial: bool,
    malformed_lines: usize,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-content-v1\0");
    digest.update((transcript.as_ref().len() as u64).to_be_bytes());
    digest.update(transcript.as_ref());
    digest.update([u8::from(partial)]);
    digest.update((malformed_lines as u64).to_be_bytes());
    digest.finalize().into()
}

#[cfg(test)]
fn subagent_content_identity_digest(
    transcript: &RedactedBytes,
    partial: bool,
    malformed_lines: usize,
) -> String {
    hex::encode(subagent_content_identity_digest_bytes(
        transcript,
        partial,
        malformed_lines,
    ))
}

struct PreparedSubagentContent<'a> {
    source: &'a DiscoveredSubagentContent,
    transcript: RedactedBytes,
    report_value: serde_json::Value,
    partial: bool,
    turn_count: usize,
    content_digest: String,
    codex_binding: Option<CodexChildBinding>,
    codex_usage: Option<CodexThreadUsage>,
}

struct AssociationProofInput<'a, 'source> {
    scope: &'a CaptureScope,
    capture_session: &'a str,
    capture_incarnation: Option<&'a str>,
    source_channel: &'a str,
    item: &'a PreparedSubagentContent<'source>,
    checkpoint_id: &'a str,
}

/// Called only after the source owner commits, or proves the same-byte current
/// leaf durable. Retaining optional usage performs no extra IO and never
/// rewrites an older checkpoint that has no usage metadata.
fn record_usage_candidate(
    summary: &mut SubagentCaptureSummary,
    input: AssociationProofInput<'_, '_>,
    revision: i64,
    usage: Option<CodexThreadUsage>,
) {
    let Some(binding) = input.item.codex_binding.as_ref() else {
        return;
    };
    if revision <= 0
        || !is_subagent_source_commitment_v2(&input.item.source.source_key)
        || binding.capture_session != input.capture_session
    {
        summary.usage_candidates_incomplete = true;
        return;
    }
    let candidate = CodexUsageCandidate {
        scope: input.scope.clone(),
        capture_session: input.capture_session.into(),
        capture_incarnation: input.capture_incarnation.map(str::to_owned),
        root: binding.root.clone(),
        parent: binding.parent.clone(),
        child: binding.child.clone(),
        source_key: input.item.source.source_key.clone(),
        content_digest: input.item.content_digest.clone(),
        checkpoint_id: input.checkpoint_id.into(),
        revision,
        usage: usage
            .unwrap_or_default()
            .after_metadata_redaction(&Redactor::new_default()),
    };
    if let Some(current) = summary
        .usage_candidates
        .iter_mut()
        .find(|current| current.source_key == candidate.source_key)
    {
        // Discovery normally deduplicates sources, but a compatibility caller
        // can supply growth and replay in one pass. Count each current leaf once.
        if candidate.revision == current.revision
            && (candidate.checkpoint_id != current.checkpoint_id
                || candidate.content_digest != current.content_digest
                || candidate.scope != current.scope
                || candidate.capture_session != current.capture_session
                || candidate.capture_incarnation != current.capture_incarnation
                || candidate.root != current.root
                || candidate.parent != current.parent
                || candidate.child != current.child)
        {
            summary.usage_candidates_incomplete = true;
        } else if candidate.revision >= current.revision {
            *current = candidate;
        } else {
            summary.usage_candidates_incomplete = true;
        }
    } else if summary.usage_candidates.len() < MAX_SUBAGENT_SOURCES_PER_CAPTURE {
        summary.usage_candidates.push(candidate);
    } else {
        summary.usage_candidates_incomplete = true;
    }
}

/// Reuse the committed source leaf, never a CID-only boundary match. The
/// observed turn comes exclusively from this invocation's projection.
async fn record_association_proof(
    conn: &DatabaseConnection,
    input: AssociationProofInput<'_, '_>,
    summary: &mut SubagentCaptureSummary,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let Some(binding) = input.item.codex_binding.as_ref() else {
        return Ok(());
    };
    let Some(turn) = binding.hook_turn.as_ref() else {
        return Ok(());
    };
    if input.source_channel != "live"
        || input.item.partial
        || !uuid::Uuid::parse_str(turn).is_ok_and(|id| matches!(id.get_version_num(), 4 | 7))
    {
        return Ok(());
    }
    if summary.association_proofs.len() >= MAX_SUBAGENT_SOURCES_PER_CAPTURE {
        bail!("child association proof limit reached; retry the capture with fewer sources");
    }
    await_subagent_precommit_read_until(
        Some(deadline.monotonic()),
        input.scope.assert_workspace_fence_live(conn),
    )
    .await?;
    let mut proof = SubagentAssociationProof {
        scope: input.scope.clone(),
        capture_session: input.capture_session.into(),
        capture_incarnation: input.capture_incarnation.map(str::to_owned),
        root: binding.root.clone(),
        child: binding.child.clone(),
        observed_turn: turn.clone(),
        source_key: input.item.source.source_key.clone(),
        content_digest: input.item.content_digest.clone(),
        content_checkpoint_id: input.checkpoint_id.into(),
        revision: 0,
    };
    if let Some(revision) =
        association_current_revision(conn, &proof, Some(deadline.monotonic())).await?
    {
        proof.revision = revision;
        summary.association_proofs.push(proof);
    }
    Ok(())
}

async fn association_current_revision<C: ConnectionTrait>(
    conn: &C,
    proof: &SubagentAssociationProof,
    deadline: Option<Instant>,
) -> Result<Option<i64>> {
    let row = await_subagent_precommit_read_until(deadline, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT c.current_revision FROM agent_subagent_content_claim c
             JOIN agent_subagent_content_revision r
               ON r.parent_session_id = c.parent_session_id AND r.provider_kind = c.provider_kind
              AND r.source_key = c.source_key AND r.content_schema_version = c.content_schema_version
              AND r.revision = c.current_revision AND r.checkpoint_id = c.current_checkpoint_id
              AND r.content_digest = c.current_digest
             JOIN agent_checkpoint cp ON cp.checkpoint_id = c.current_checkpoint_id
              AND cp.session_id = c.parent_session_id AND cp.scope = 'subagent'
             JOIN agent_subagent_link l ON l.content_checkpoint_id = cp.checkpoint_id
              AND l.parent_session_id = c.parent_session_id
             JOIN agent_session s ON s.session_id = c.parent_session_id
             WHERE c.parent_session_id = ? AND c.provider_kind = 'codex'
               AND c.source_key = ? AND c.content_schema_version = ?
               AND c.state = 'idle' AND c.owner IS NULL AND c.lease_expires_at IS NULL
               AND c.attempt_digest IS NULL AND c.attempt_checkpoint_id IS NULL
               AND c.current_revision > 0 AND c.current_checkpoint_id = ? AND c.current_digest = ?
               AND l.stable_subagent_id = ? AND s.scope_state = 'scoped'
               AND s.repo_id = ? AND s.worktree_id = ?
               AND s.workspace_id IS ? AND s.workspace_fence IS ?
               AND s.agent_kind = 'codex' AND s.provider_session_id = ?
               AND json_extract(s.metadata_json, '$.capture_incarnation') IS ?
               AND NOT EXISTS (SELECT 1 FROM agent_import_tombstone t
                   WHERE t.agent_kind = s.agent_kind AND t.provider_session_id = s.provider_session_id)",
            [proof.capture_session.clone().into(), proof.source_key.clone().into(),
             SUBAGENT_CONTENT_SCHEMA_VERSION.into(), proof.content_checkpoint_id.clone().into(),
             proof.content_digest.clone().into(), proof.child.clone().into(),
             proof.scope.repo_id.clone().into(), proof.scope.worktree_id.clone().into(),
             proof.scope.workspace_id.clone().into(), proof.scope.workspace_fence.into(),
             proof.root.clone().into(), proof.capture_incarnation.clone().into()],
        )).await.context("recheck exact child content claim before association")
    }).await?;
    row.map(|row| row.try_get_by::<i64, _>("current_revision"))
        .transpose()
        .context("decode exact child content revision")
}

impl SubagentBoundaryAssociations {
    /// Called only after the original boundary catalog companion, inside its
    /// traces/ref transaction. Races with another revision stay unresolved.
    pub(crate) async fn apply(
        &self,
        txn: &DatabaseTransaction,
        traces: &TracesCommitCtx,
        context: SubagentBoundaryAssociationContext<'_>,
    ) -> Result<()> {
        if self.proofs.is_empty()
            || self.boundary_checkpoint_id.as_deref() != Some(context.boundary_checkpoint_id)
        {
            return Ok(());
        }
        let deadline = context.deadline.map(|deadline| deadline.monotonic());
        ensure_before_deadline(deadline)?;
        await_subagent_precommit_read_until(
            deadline,
            context.scope.assert_workspace_fence_live(txn),
        )
        .await?;
        for proof in &self.proofs {
            if &proof.scope != context.scope || proof.capture_session != context.capture_session {
                bail!("child association capture scope changed; retry from the current workspace");
            }
            let current_parent = await_subagent_precommit_read_until(deadline, async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 AS current_parent FROM agent_session s
                     WHERE s.session_id = ? AND s.agent_kind = 'codex' AND s.provider_session_id = ?
                       AND s.scope_state = 'scoped' AND s.repo_id = ? AND s.worktree_id = ?
                       AND s.workspace_id IS ? AND s.workspace_fence IS ?
                       AND json_extract(s.metadata_json, '$.capture_incarnation') IS ?",
                    [
                        proof.capture_session.clone().into(),
                        proof.root.clone().into(),
                        proof.scope.repo_id.clone().into(),
                        proof.scope.worktree_id.clone().into(),
                        proof.scope.workspace_id.clone().into(),
                        proof.scope.workspace_fence.into(),
                        proof.capture_incarnation.clone().into(),
                    ],
                ))
                .await
                .context("recheck child association capture incarnation")
            })
            .await?;
            if current_parent.is_none() {
                bail!(
                    "child association belongs to an earlier capture; retry from the current workspace"
                );
            }
            if association_current_revision(txn, proof, deadline).await? != Some(proof.revision) {
                continue;
            }
            // Only pure reads may be cancelled. Once this UPDATE is
            // dispatched, await it and let the enclosing transaction's final
            // deadline authorization decide whether any mutation can commit.
            ensure_before_deadline(deadline)?;
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_subagent_link
                     SET link_state = 'resolved', boundary_checkpoint_id = ?,
                         sync_revision = sync_revision + 1, updated_at = ?
                     WHERE content_checkpoint_id = ? AND parent_session_id = ?
                       AND stable_subagent_id = ? AND link_state = 'unresolved'
                       AND boundary_checkpoint_id IS NULL
                       AND EXISTS (SELECT 1 FROM agent_checkpoint b
                           WHERE b.checkpoint_id = ? AND b.session_id = ? AND b.scope = 'subagent'
                             AND b.subagent_session_id IS NULL AND b.traces_commit = ?
                             AND b.tree_oid = ? AND b.metadata_blob_oid = ?)",
                [
                    context.boundary_checkpoint_id.into(),
                    Utc::now().timestamp_millis().into(),
                    proof.content_checkpoint_id.clone().into(),
                    proof.capture_session.clone().into(),
                    proof.child.clone().into(),
                    context.boundary_checkpoint_id.into(),
                    context.capture_session.into(),
                    traces.commit_hash.clone().into(),
                    traces.tree_oid.clone().into(),
                    traces.metadata_blob_oid.clone().into(),
                ],
            ))
            .await
            .context("associate exact child revision with its current boundary")?;
        }
        ensure_before_deadline(deadline)
    }
}

/// Read a current receipt fence before auxiliary replay IO. Release the short
/// database transaction before discovery/projection or object verification.
pub(crate) async fn prepare_child_boundary_replay(
    conn: &DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    deadline: CaptureCommitDeadline,
) -> Result<Option<ChildBoundaryReplayFence>> {
    let txn =
        begin_subagent_write_transaction_until(conn, deadline, "prepare child boundary replay")
            .await?;
    let result = await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        child_boundary_replay_fence(&txn, request)
            .await
            .map_err(anyhow::Error::new)
    })
    .await;
    txn.rollback()
        .await
        .context("release child boundary replay preflight transaction")?;
    result.context("verify the current child boundary receipt; inspect with `libra agent doctor` before retrying")
}

/// Associate fresh live content with an already durable boundary without
/// rewriting its immutable objects, appending traces, or reopening a receipt.
pub(crate) async fn replay_child_boundary_associations(
    conn: &DatabaseConnection,
    storage_root: &Path,
    associations: &SubagentBoundaryAssociations,
    fence: &ChildBoundaryReplayFence,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    if associations.proofs.is_empty() {
        return Ok(());
    }
    if associations.boundary_checkpoint_id.as_deref() != Some(fence.checkpoint_id()) {
        bail!("child boundary replay does not match the current capture action");
    }
    let until = Some(deadline.monotonic());
    let context = fence.context();
    let row = await_subagent_precommit_read_until(until, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT traces_commit, tree_oid, metadata_blob_oid FROM agent_checkpoint
             WHERE checkpoint_id = ? AND session_id = ? AND scope = 'subagent'
               AND subagent_session_id IS NULL LIMIT 1",
            [fence.checkpoint_id().into(), context.session_id().into()],
        ))
        .await
        .context("read existing child boundary for replay")
    })
    .await?
    .context("child boundary is not durable yet; retry this hook after checkpoint recovery")?;
    let traces = TracesCommitCtx {
        commit_hash: row.try_get_by("traces_commit")?,
        tree_oid: row.try_get_by("tree_oid")?,
        metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
    };
    let head = traces_head(conn, until)
        .await?
        .context("child boundary traces ref is missing; inspect with `libra agent doctor`")?;
    await_subagent_precommit_read_until(until, async {
        history::checkpoint_snapshot_durable_oids(
            conn,
            storage_root,
            &[history::CheckpointDurabilitySpec {
                checkpoint_id: fence.checkpoint_id(),
                traces_commit: &traces.commit_hash,
                tree_oid: &traces.tree_oid,
                metadata_blob_oid: &traces.metadata_blob_oid,
            }],
            until,
        )
        .await
        .context("verify existing child boundary objects and traces reachability")
    })
    .await?;
    let txn =
        begin_subagent_write_transaction_until(conn, deadline, "replay child boundary").await?;
    let result = async {
        if !await_subagent_precommit_read_until(until, async {
            fence.is_current(&txn).await.map_err(anyhow::Error::new)
        }).await? {
            bail!("child boundary replay was superseded by a newer capture action; retry from the current session");
        }
        if traces_head(&txn, until).await?.as_deref() != Some(head.as_str()) {
            bail!("traces changed during child boundary replay; retry the hook");
        }
        let unchanged = await_subagent_precommit_read_until(until, async {
            txn.query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS unchanged FROM agent_checkpoint
                 WHERE checkpoint_id = ? AND session_id = ? AND scope = 'subagent'
                   AND subagent_session_id IS NULL AND traces_commit = ?
                   AND tree_oid = ? AND metadata_blob_oid = ? LIMIT 1",
                [fence.checkpoint_id().into(), context.session_id().into(),
                 traces.commit_hash.clone().into(), traces.tree_oid.clone().into(),
                 traces.metadata_blob_oid.clone().into()],
            )).await.context("recheck existing child boundary identity")
        }).await?;
        if unchanged.is_none() {
            bail!("child boundary changed during replay; retry after checkpoint recovery");
        }
        associations.apply(&txn, &traces, SubagentBoundaryAssociationContext {
            scope: context.scope(),
            capture_session: context.session_id(),
            boundary_checkpoint_id: fence.checkpoint_id(),
            deadline: Some(deadline),
        }).await
    }.await;
    if let Err(error) = result {
        txn.rollback()
            .await
            .context("roll back child boundary replay")?;
        return Err(error);
    }
    commit_subagent_transaction_until(
        txn,
        context.scope(),
        Some(deadline),
        "commit child boundary replay association",
    )
    .await
}

async fn traces_head<C: ConnectionTrait>(
    conn: &C,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    await_subagent_precommit_read_until(deadline, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .context("read refs/libra/traces for subagent durability proof")
    })
    .await?
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
        let row = await_subagent_precommit_read_until(deadline, async {
            conn.query_one_raw(Statement::from_sql_and_values(
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
            .context("load unchanged subagent content for batch durability proof")
        })
        .await?;
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
    let head_before = traces_head(conn, deadline)
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
    let head_after = traces_head(conn, deadline).await?;
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
///
/// Kept for in-crate compatibility fixtures; production paths must call the
/// scope-carrying variant below.
#[allow(dead_code)]
pub(crate) async fn capture_discovered_subagent_contents(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<SubagentCaptureSummary> {
    // Compatibility seam for in-process tests and older callers. Real hook
    // and import paths pass the ingress-resolved scope through the explicit
    // API below; this fallback still constructs a typed main-worktree scope
    // instead of accepting an unscoped mutable write.
    let scope = await_subagent_precommit_read_until(
        deadline.map(CaptureCommitDeadline::monotonic),
        CaptureScope::main_for_connection(conn),
    )
    .await?;
    capture_discovered_subagent_contents_with_scope(
        conn,
        &scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
    )
    .await
}

/// Scope-aware child-content capture. The supplied scope must match the
/// parent session's durable ownership tuple exactly; a workspace lease is
/// rechecked at each mutation and again inside the final traces ref-CAS
/// transaction.
pub(crate) async fn capture_discovered_subagent_contents_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<SubagentCaptureSummary> {
    capture_subagent_invocation(SubagentCaptureInvocation {
        conn,
        scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
        preparation: SubagentPreparation::Ungated,
    })
    .await
}

/// A pending parent receipt supplies the exact action/scope fence. This uses
/// the same content writer as ordinary live/import capture, with no executor.
pub(crate) async fn capture_subagent_contents_for_checkpoint(
    conn: &DatabaseConnection,
    storage_root: &Path,
    request: &CaptureCatalogApplyRequest,
    discovery: &SubagentDiscovery,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<SubagentCaptureSummary> {
    capture_subagent_invocation(SubagentCaptureInvocation {
        conn,
        scope: request.scope(),
        storage_root,
        parent_session_id: request.session().session_id(),
        sources: &discovery.sources,
        source_channel: "live",
        deadline,
        preparation: SubagentPreparation::Checkpoint {
            request,
            finish: discovery.availability
                == super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available,
        },
    })
    .await
    .map(|summary| summary.with_discovery_usage(discovery))
}

/// Both automatic recovery and doctor reuse this bounded Root driver. The
/// factory is the original registration; no provider identity is rebuilt.
pub(crate) async fn recover_terminal_children(
    conn: &DatabaseConnection,
    storage_root: &Path,
    recovery: &CaptureCatalogFinalizerRecovery,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let result = recover_terminal_children_with_binding(
        conn,
        storage_root,
        recovery,
        deadline,
        super::observed_agents::live_capture::LiveCaptureBinding::for_stored,
    )
    .await;
    match result {
        Err(error) if capture_deadline_exhausted(&error) => Err(anyhow::Error::from(
            CaptureCatalogError::ChildCheckpointsPending,
        )
        .context(
            "terminal child recovery retains its original pending state after its budget expired",
        )),
        result => result,
    }
}

async fn recover_terminal_children_with_binding(
    conn: &DatabaseConnection,
    storage_root: &Path,
    recovery: &CaptureCatalogFinalizerRecovery,
    deadline: CaptureCommitDeadline,
    binding_for: impl FnOnce(
        super::observed_agents::AgentKind,
    ) -> super::observed_agents::live_capture::LiveCaptureBinding,
) -> Result<()> {
    use super::observed_agents::live_capture::{
        LiveCaptureContext, StoredSubagentSelection, SubagentDiscoveryLimits,
    };
    let txn =
        begin_subagent_write_transaction_until(conn, deadline, "prepare terminal children").await?;
    let prepared = child_preparation_for_recovery(&txn, recovery, deadline).await;
    let prepared = match prepared {
        Ok(value) => value,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error.into());
        }
    };
    let Some(fence) = prepared else {
        txn.rollback()
            .await
            .context("release settled or legacy child recovery snapshot")?;
        return Ok(());
    };
    txn.rollback()
        .await
        .context("release original child recovery preparation snapshot")?;
    let context = fence.context();
    let kind = super::observed_agents::AgentKind::from_db_str(context.agent_kind())
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let binding = binding_for(kind);
    let Some(port) = binding.subagent_discovery() else {
        return Err(CaptureCatalogError::ChildCheckpointsPending.into());
    };
    let _publication = crate::internal::maintenance_lock::MaintenanceLock::shared_until(
        storage_root,
        deadline.monotonic(),
    )
    .await?
    .ok_or(CaptureCatalogError::ChildCheckpointsPending)?;
    let cwd = Path::new(context.working_dir());
    let discovery = match port
        .discover_stored(
            &LiveCaptureContext {
                libra_session_id: context.session_id(),
                provider_session_id: context.provider_session_id(),
                verified_cwd: cwd,
            },
            StoredSubagentSelection::TerminalRecoveryRoot,
            SubagentDiscoveryLimits {
                byte_budget: TRANSCRIPT_READ_HARD_CAP_BYTES,
                source_limit: MAX_SUBAGENT_SOURCES_PER_CAPTURE,
            },
            deadline.monotonic(),
        )
        .await
    {
        Ok(discovery) => discovery,
        Err(_) => return Err(CaptureCatalogError::ChildCheckpointsPending.into()),
    };
    // Availability is distinct from bounded partial data. An unavailable
    // environment cannot poison an already prepared gate; its flag was not
    // changed. Any validated partial sources remain ordinary capture data.
    if discovery.sources.is_empty()
        && discovery.availability
            == super::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable
    {
        return Err(CaptureCatalogError::ChildCheckpointsPending.into());
    }
    let result = capture_subagent_invocation(SubagentCaptureInvocation {
        conn,
        scope: context.scope(),
        storage_root,
        parent_session_id: context.session_id(),
        sources: &discovery.sources,
        source_channel: "live",
        deadline: Some(deadline),
        preparation: SubagentPreparation::Recovery {
            fence: &fence,
            finish: discovery.availability
                == super::observed_agents::live_capture::SubagentDiscoveryAvailability::Available,
        },
    })
    .await;
    if let Err(error) = result {
        if capture_deadline_exhausted(&error) {
            return Err(
                anyhow::Error::from(CaptureCatalogError::ChildCheckpointsPending)
                    .context("terminal child recovery remains pending within its original budget"),
            );
        }
        return Err(error);
    }
    let txn =
        begin_subagent_write_transaction_until(conn, deadline, "check terminal children").await?;
    let pending = child_recovery_is_pending(&txn, recovery, deadline).await;
    txn.rollback()
        .await
        .context("release terminal child completion snapshot")?;
    if pending? {
        return Err(CaptureCatalogError::ChildCheckpointsPending.into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SubagentPreparation<'a> {
    Ungated,
    Checkpoint {
        request: &'a CaptureCatalogApplyRequest,
        finish: bool,
    },
    Recovery {
        fence: &'a ChildPreparationFence,
        finish: bool,
    },
}

async fn capture_subagent_invocation(
    invocation: SubagentCaptureInvocation<'_>,
) -> Result<SubagentCaptureSummary> {
    let mut summary = SubagentCaptureSummary {
        discovered: invocation.sources.len(),
        ..SubagentCaptureSummary::default()
    };
    let has_codex_sources = invocation
        .sources
        .iter()
        .any(|source| source.provider_kind == "codex");
    if let Err(source) = capture_discovered_subagent_contents_inner(invocation, &mut summary).await
    {
        summary.usage_candidates_incomplete |= has_codex_sources;
        return Err(SubagentCaptureProgressError { summary, source }.into());
    }
    Ok(summary)
}

/// Shared immutable inputs for one child-content capture. Grouping these
/// keeps the progress-bearing worker below from accumulating independent
/// parameters as capture fencing evolves.
struct SubagentCaptureInvocation<'a> {
    conn: &'a DatabaseConnection,
    scope: &'a CaptureScope,
    storage_root: &'a Path,
    parent_session_id: &'a str,
    sources: &'a [DiscoveredSubagentContent],
    source_channel: &'a str,
    deadline: Option<CaptureCommitDeadline>,
    preparation: SubagentPreparation<'a>,
}

async fn capture_discovered_subagent_contents_inner(
    invocation: SubagentCaptureInvocation<'_>,
    summary: &mut SubagentCaptureSummary,
) -> Result<()> {
    let SubagentCaptureInvocation {
        conn,
        scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
        preparation,
    } = invocation;
    if !matches!(source_channel, "live" | "import") {
        bail!("invalid subagent content source channel");
    }
    let requires_preparation = match preparation {
        SubagentPreparation::Ungated => false,
        SubagentPreparation::Checkpoint { request, .. } => request.child_checkpoints_required(),
        SubagentPreparation::Recovery { .. } => true,
    };
    if requires_preparation && deadline.is_none() {
        return Err(CaptureCatalogError::InvalidRequest.into());
    }
    // Programmatic callers predating M5 did not supply a deadline. Bound
    // contention waits anyway so an abandoned live lease can never turn this
    // API into an unbounded hang.
    let effective_deadline = match deadline {
        Some(deadline) => deadline,
        // This compatibility ingress establishes both clock halves together.
        // Lower layers must never reconstruct the SQLite half from a later
        // monotonic Instant.
        None => CaptureCommitDeadline::from_budget(Duration::from_secs(5))
            .context("compute bounded deadline for subagent content projection")?,
    };
    let projection_deadline = effective_deadline.monotonic();
    if requires_preparation && sources.len() > MAX_SUBAGENT_SOURCES_PER_CAPTURE {
        return Err(CaptureCatalogError::ChildGateCapacity.into());
    }
    let recovery_pass = matches!(preparation, SubagentPreparation::Recovery { .. });
    let finish_preparation = match preparation {
        SubagentPreparation::Ungated => false,
        SubagentPreparation::Checkpoint { finish, .. }
        | SubagentPreparation::Recovery { finish, .. } => finish,
    };
    let preparation_fence = if requires_preparation {
        let txn = begin_subagent_write_transaction_until(
            conn,
            effective_deadline,
            "load child preparation fence",
        )
        .await?;
        let fence = match preparation {
            SubagentPreparation::Checkpoint { request, .. } => {
                child_preparation_fence(&txn, request).await
            }
            SubagentPreparation::Recovery { fence, .. } => fence
                .assert_current(&txn)
                .await
                .map(|()| Some(fence.clone())),
            SubagentPreparation::Ungated => Err(CaptureCatalogError::InvalidRequest),
        };
        txn.rollback()
            .await
            .context("release child preparation fence snapshot")?;
        fence?
    } else {
        None
    };

    await_subagent_precommit_read_until(
        Some(projection_deadline),
        scope.assert_workspace_fence_live(conn),
    )
    .await
    .context("verify capture workspace lease before preparing subagent content")?;
    let parent = await_subagent_precommit_read_until(Some(projection_deadline), async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT agent_kind, provider_session_id, working_dir,
                    json_extract(metadata_json, '$.capture_incarnation') AS capture_incarnation,
                    scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_session WHERE session_id = ?",
            [parent_session_id.into()],
        ))
        .await
        .context("verify subagent content parent session")
    })
    .await?
    .context("subagent content parent session does not exist")?;
    let parent_agent_kind: String = parent.try_get_by("agent_kind")?;
    let parent_provider_session_id: String = parent.try_get_by("provider_session_id")?;
    let parent_working_dir: String = parent.try_get_by("working_dir")?;
    let capture_incarnation: Option<String> = parent.try_get_by("capture_incarnation")?;
    let parent_scope_state: String = parent.try_get_by("scope_state")?;
    let parent_repo_id: Option<String> = parent.try_get_by("repo_id")?;
    let parent_worktree_id: Option<String> = parent.try_get_by("worktree_id")?;
    let parent_workspace_id: Option<String> = parent.try_get_by("workspace_id")?;
    let parent_workspace_fence: Option<i64> = parent.try_get_by("workspace_fence")?;
    let parent_scope = CaptureScope {
        repo_id: parent_repo_id.context(
            "subagent content parent has no repository scope; run `libra worktree doctor` before retrying",
        )?,
        worktree_id: parent_worktree_id.context(
            "subagent content parent has no worktree scope; run `libra worktree doctor` before retrying",
        )?,
        workspace_id: parent_workspace_id,
        workspace_fence: parent_workspace_fence,
    };
    if parent_scope_state != "scoped" || parent_scope != *scope {
        bail!(
            "subagent content parent belongs to a legacy or different capture scope; \
             refusing to mutate it — inspect it with `libra worktree doctor` before retrying"
        );
    }
    await_subagent_precommit_read_until(
        Some(projection_deadline),
        scope.assert_provider_session_compatible(conn, &parent_provider_session_id),
    )
    .await
    .context("verify subagent content parent capture scope")?;
    let parent_working_dir = PathBuf::from(parent_working_dir)
        .canonicalize()
        .context("canonicalize scoped subagent content parent worktree")?;
    let mut incarnation_sources = sources.to_vec();
    for source in &mut incarnation_sources {
        ensure_before_deadline(Some(projection_deadline))?;
        validate_source_identity(&source.provider_kind, &source.source_key)?;
        let legacy_source_key =
            source_key_for_incarnation(&source.source_key, capture_incarnation.as_deref())?;
        reject_matching_legacy_subagent_claim(
            conn,
            parent_session_id,
            &source.provider_kind,
            &legacy_source_key,
            Some(projection_deadline),
        )
        .await?;
        ensure_before_deadline(Some(projection_deadline))?;
        let preimage = subagent_source_preimage(
            &source.provider_kind,
            &source.source_key,
            capture_incarnation.as_deref(),
        )?;
        let commitment = derive_capture_source_commitment_in_scope_until(
            conn,
            scope,
            storage_root,
            &parent_working_dir,
            CaptureSourceCommitmentDomain::SubagentSourceV2,
            &preimage,
            projection_deadline,
        )
        .await
        .context("derive scoped subagent source commitment")?;
        if !is_subagent_source_commitment_v2(&commitment) {
            bail!("derive scoped subagent source commitment returned an invalid proof");
        }
        source.legacy_source_key = Some(legacy_source_key);
        source.source_key = commitment;
    }
    summary.discovered = incarnation_sources.len();
    let mut prepared = Vec::with_capacity(incarnation_sources.len());
    for source in &incarnation_sources {
        ensure_before_deadline(Some(projection_deadline))?;
        if !is_subagent_source_commitment_v2(&source.source_key) {
            bail!("subagent source commitment was not prepared for durable capture");
        }
        if source.provider_kind != parent_agent_kind {
            bail!(
                "subagent content provider does not match its parent capture session (fail-closed)"
            );
        }
        let SafeSubagentProjection {
            transcript,
            report_value,
            partial,
            turn_count,
            content_digest,
            codex_binding,
            codex_usage,
        } = if source.codex_binding.is_some() {
            safe_content_projection_with_usage_until(
                source,
                effective_deadline,
                projection_deadline,
            )
            .await?
        } else {
            safe_content_projection_until(source, projection_deadline).await?
        };
        if codex_binding
            .as_ref()
            .is_some_and(|binding| binding.capture_session != parent_session_id)
        {
            bail!(
                "Codex child source does not match this capture session; retry from the original workspace"
            );
        }
        prepared.push(PreparedSubagentContent {
            source,
            transcript,
            report_value,
            partial,
            turn_count,
            content_digest: hex::encode(content_digest),
            codex_binding,
            codex_usage,
        });
    }
    let mut durability_proof = build_unchanged_durability_proof(
        conn,
        storage_root,
        parent_session_id,
        &prepared,
        Some(projection_deadline),
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
    'contents: for index in 0..prepared.len() {
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
        let mut saw_live_writer = false;
        let reservation = loop {
            ensure_before_deadline(Some(projection_deadline))
                .context(SUBAGENT_LIVE_WRITER_RETRY)?;
            let reservation = reserve_source_prepared(
                conn,
                scope,
                parent_session_id,
                source,
                SourceReservationAttempt {
                    attempt: ReservationAttempt {
                        content_digest,
                        checkpoint_id: &checkpoint_id,
                        owner: &owner,
                    },
                    preparation: preparation_fence.as_ref(),
                },
                &durability_proof,
                effective_deadline,
            )
            .await
            .map_err(|error| {
                if saw_live_writer {
                    live_writer_retry(error)
                } else {
                    error
                }
            })?;
            if let Some(requirement) = reservation.requirement {
                requirement.merge_into(&mut summary.child_requirements);
            }
            match reservation.outcome {
                ReservationOutcome::Inflight { lease_expires_at } => {
                    subagent_content_test_failpoint("after_inflight_requirement")?;
                    if recovery_pass
                        || (source_channel == "import" && source.codex_binding.is_some())
                    {
                        // Import and recovery need time to settle their
                        // own control outcome. The active child owner keeps
                        // its lease; the enrolled floor remains pending.
                        summary.skipped_inflight = summary.skipped_inflight.saturating_add(1);
                        summary.usage_candidates_incomplete |= source.codex_binding.is_some();
                        continue 'contents;
                    }
                    saw_live_writer = true;
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
                        Some(projection_deadline),
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
            ReservationOutcome::Unchanged {
                checkpoint_exists,
                revision,
            } => {
                if checkpoint_exists {
                    if let Some(existing) =
                        durability_proof.source_checkpoint(source, content_digest)
                    {
                        record_usage_candidate(
                            summary,
                            AssociationProofInput {
                                scope,
                                capture_session: parent_session_id,
                                capture_incarnation: capture_incarnation.as_deref(),
                                source_channel,
                                item,
                                checkpoint_id: existing,
                            },
                            revision,
                            item.codex_usage.clone(),
                        );
                    } else if item.codex_binding.is_some() {
                        summary.usage_candidates_incomplete = true;
                    }
                    refresh_current_link_prepared(
                        conn,
                        scope,
                        parent_session_id,
                        source,
                        effective_deadline,
                        preparation_fence.as_ref(),
                    )
                    .await?;
                    if let Some(existing) =
                        durability_proof.source_checkpoint(source, content_digest)
                    {
                        record_association_proof(
                            conn,
                            AssociationProofInput {
                                scope,
                                capture_session: parent_session_id,
                                capture_incarnation: capture_incarnation.as_deref(),
                                source_channel,
                                item,
                                checkpoint_id: existing,
                            },
                            summary,
                            effective_deadline,
                        )
                        .await?;
                    }
                }
                if !checkpoint_exists && item.codex_binding.is_some() {
                    summary.usage_candidates_incomplete = true;
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
                scope,
                ReservationRelease {
                    preparation: preparation_fence.as_ref(),
                    parent_session_id,
                    source,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                    fence_token,
                },
                effective_deadline,
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
        if let Err(error) =
            register_traces_write_attempt_with_scope(conn, scope, &marker, effective_deadline).await
        {
            let cleanup = release_reservation(
                conn,
                scope,
                ReservationRelease {
                    preparation: preparation_fence.as_ref(),
                    parent_session_id,
                    source,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                    fence_token,
                },
                effective_deadline,
            )
            .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    preserve_cleanup_error(error, "release subagent content reservation", cleanup)
                }
            });
        }

        let mut durable_usage = item.codex_usage.clone();
        let written: Result<_> = async {
            subagent_content_test_failpoint("after_marker")?;
            let created_at = Utc::now().timestamp();
            let parent_commit = current_parent_commit(conn, Some(projection_deadline)).await?;
            let mut metadata_value = serde_json::json!({
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
            if let Some(binding) = item.codex_binding.as_ref() {
                // The generic id belongs to boundary evidence. Retain the
                // source's identities without authorizing a CID-only match.
                metadata_value["subagent_session_id"] = serde_json::Value::Null;
                metadata_value["subagent"]["link_state"] = "unresolved".into();
                metadata_value["subagent"]["source_identity"] = serde_json::json!({
                    "root": binding.root,
                    "parent": binding.parent,
                    "child": binding.child,
                });
                metadata_value["subagent"]["capture_incarnation"] =
                    serde_json::json!(capture_incarnation);
                metadata_value["subagent"]["raw_byte_count"] =
                    serde_json::json!(source.bytes.len());
                let usage = durable_usage.get_or_insert_with(CodexThreadUsage::default);
                metadata_value["subagent"]["usage"] = serde_json::to_value(usage)
                    .context("serialize ordinary child usage metadata")?;
            }
            let metadata = redacted_subagent_metadata(&mut metadata_value, &mut durable_usage)?;
            let report_bytes = serde_json::to_vec_pretty(&report_value)
                .context("serialize subagent content redaction report")?;
            let (report, _) = Redactor::new_default().redact(&report_bytes);
            let empty_events = RedactedBytes::new_unchecked(Vec::new());

            let objects_dir = storage_root.join("objects");
            std::fs::create_dir_all(&objects_dir)
                .context("create objects directory for subagent content")?;
            let manager = HistoryManager::new_with_ref(
                Arc::new(
                    ClientStorage::open_init_storage(objects_dir).map_err(|error| {
                        anyhow!(
                            "failed to read storage configuration for {}",
                            error.variable()
                        )
                    })?,
                ),
                storage_root.to_path_buf(),
                Arc::new(conn.clone()),
                crate::internal::branch::TRACES_BRANCH,
            );
            let plan = ContentCommitPlan {
                preparation: preparation_fence.clone(),
                capture_scope: scope.clone(),
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
                codex_binding: item.codex_binding.clone(),
                created_at,
                deadline: effective_deadline,
            };
            let append_deadline =
                final_subagent_append_deadline(deadline, Some(effective_deadline));
            observe_subagent_final_append_deadline(append_deadline);
            manager
                .append_checkpoint_commit(CheckpointCommitParams {
                    reasoning_artifacts: &[],
                    checkpoint_id: &checkpoint_id,
                    session_id: parent_session_id,
                    marker_generation,
                    capture_scope: Some(scope),
                    agent_kind: &source.provider_kind,
                    parent_commit: parent_commit.as_deref(),
                    scope: CheckpointScope::Subagent,
                    tool_use_id: None,
                    metadata_json: &metadata,
                    transcript_redacted: transcript,
                    lifecycle_events_jsonl: &empty_events,
                    redaction_report_json: &report,
                    txn_extra: Some(&plan),
                    // The compatibility wrapper synthesizes a short bounded
                    // deadline when legacy in-process callers pass `None`.
                    // Carry that same bound through the final ref/catalog
                    // transaction rather than letting only the earlier
                    // reservation/projection phases be deadline-aware.
                    deadline: append_deadline,
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
                    scope,
                    ReservationRelease {
                        preparation: preparation_fence.as_ref(),
                        parent_session_id,
                        source,
                        checkpoint_id: &checkpoint_id,
                        owner: &owner,
                        fence_token,
                    },
                    effective_deadline,
                )
                .await
                {
                    error = preserve_cleanup_error(
                        error,
                        "release subagent content reservation",
                        cleanup,
                    );
                }
                if let Err(cleanup) = clear_non_cleanup_traces_inflight_marker_with_scope(
                    conn,
                    scope,
                    parent_session_id,
                    &checkpoint_id,
                    marker_generation,
                    effective_deadline,
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
        if clear_non_cleanup_traces_inflight_marker_with_scope(
            conn,
            scope,
            parent_session_id,
            &checkpoint_id,
            &written.marker_generation,
            effective_deadline,
        )
        .await
        .is_err()
        {
            tracing::warn!(
                cleanup_reason = "clear_committed_subagent_marker_failed",
                "failed to clear committed subagent content in-flight marker"
            );
        }
        summary.checkpoints_written += 1;
        record_usage_candidate(
            summary,
            AssociationProofInput {
                scope,
                capture_session: parent_session_id,
                capture_incarnation: capture_incarnation.as_deref(),
                source_channel,
                item,
                checkpoint_id: &checkpoint_id,
            },
            revision,
            durable_usage,
        );
        record_association_proof(
            conn,
            AssociationProofInput {
                scope,
                capture_session: parent_session_id,
                capture_incarnation: capture_incarnation.as_deref(),
                source_channel,
                item,
                checkpoint_id: &checkpoint_id,
            },
            summary,
            effective_deadline,
        )
        .await?;
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
    // An unavailable pass can still return verified bodies. Save them and
    // their floors, but retain the original preparation barrier.
    if finish_preparation && let Some(fence) = &preparation_fence {
        let txn = begin_subagent_write_transaction_until(
            conn,
            effective_deadline,
            "finish child preparation",
        )
        .await?;
        if let Err(error) = fence.finish(&txn, effective_deadline).await {
            txn.rollback().await.ok();
            return Err(error.into());
        }
        commit_subagent_transaction_until(
            txn,
            scope,
            Some(effective_deadline),
            "commit prepared child requirements",
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use sea_orm::DatabaseConnection;
    use serial_test::serial;

    use super::*;
    use crate::{
        internal::{
            ai::observed_agents::{ObservedAgent, SubagentAwareExtractor},
            config::ConfigKv,
        },
        utils::client_storage::{ObjectIndexTestFaults, install_test_object_index_faults},
    };

    #[cfg(unix)]
    fn write_stalled_helper(path: &Path, pid_file: &Path) {
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled helper");
        let mut permissions = std::fs::metadata(path)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("mark helper executable");
    }

    #[cfg(unix)]
    fn write_pipe_holding_descendant_helper(path: &Path, descendant_file: &Path) {
        let descendant_path = descendant_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s\\n' \"$!\" > '{descendant_path}'\nexit 0\n"
            ),
        )
        .expect("write forking helper");
        let mut permissions = std::fs::metadata(path)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("mark helper executable");
    }

    #[cfg(unix)]
    async fn wait_for_stalled_helper_pid(pid_file: &Path) -> Option<libc::pid_t> {
        // A process-isolated test runner can delay the shell fixture's first
        // scheduling slice. This is readiness synchronization; the deadline
        // assertion begins only after the child is observable.
        for _ in 0..500 {
            if let Ok(value) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = value.trim().parse::<libc::pid_t>()
            {
                return Some(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    #[cfg(unix)]
    async fn wait_for_stalled_helper_reap(pid: libc::pid_t) -> bool {
        for _ in 0..200 {
            // SAFETY: signal zero probes the exact test child PID and does
            // not deliver a signal.
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[test]
    fn unregistered_host_disables_unbounded_discovery() {
        assert!(
            helper_program().is_none(),
            "an embedded test host must not infer a private helper executable"
        );
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

    fn discovery_wire_source_key() -> String {
        format!("source/sha256/{}", "a".repeat(64))
    }

    fn discovery_wire_fixture(raw: &[u8]) -> SubagentDiscovery {
        SubagentDiscovery {
            sources: vec![DiscoveredSubagentContent::fixture(
                "claude_code",
                &discovery_wire_source_key(),
                raw,
                None,
            )],
            bytes_read: u64::try_from(raw.len()).expect("fixture byte count"),
            warning: None,
            incomplete: false,
            ..SubagentDiscovery::default()
        }
    }

    fn discovery_frame_header_end(frame: &[u8]) -> usize {
        assert!(frame.len() >= SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES);
        let header_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES + header_len
    }

    fn decode_discovery_wire_for_test(frame: Vec<u8>) -> Result<SubagentDiscovery> {
        decode_subagent_discovery_helper_frame(
            frame,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
            Instant::now() + Duration::from_secs(2),
        )
    }

    #[test]
    fn discovery_helper_frame_keeps_raw_segments_out_of_metadata_and_within_acf03_budget() {
        let raw = b"provider-native-secret-that-must-not-enter-json-metadata";
        let frame = encode_subagent_discovery_helper_success(discovery_wire_fixture(raw))
            .expect("encode direct-raw discovery frame");
        let header_end = discovery_frame_header_end(&frame);
        assert!(
            header_end
                <= SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
                    + usize::try_from(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP)
                        .expect("header cap fits usize"),
            "metadata header must remain separately bounded"
        );
        assert!(
            !frame[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end]
                .windows(raw.len())
                .any(|window| window == raw),
            "provider-native bytes must not be JSON/base64 encoded into the helper metadata"
        );
        assert_eq!(
            &frame[header_end..],
            raw,
            "the payload tail must carry direct raw source bytes"
        );
        let decoded = decode_discovery_wire_for_test(frame).expect("decode direct-raw frame");
        assert_eq!(decoded.sources.len(), 1);
        assert_eq!(decoded.sources[0].bytes, raw);

        // Parent peak: one preallocated pipe frame (including the one-byte
        // over-cap sentinel), the completed source Vecs, and the owned small
        // metadata header parsed by serde. At the 16 MiB source cap this is
        // 32 MiB + 128 KiB + 1, safely below ACF-03's 2.5x (40 MiB) limit.
        let parent_peak = SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
            .saturating_add(1)
            .saturating_add(TRANSCRIPT_READ_HARD_CAP_BYTES)
            .saturating_add(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP);
        assert!(
            parent_peak <= TRANSCRIPT_READ_HARD_CAP_BYTES.saturating_mul(5) / 2,
            "direct-raw discovery framing must stay within ACF-03's 2.5x working-set budget"
        );
    }

    #[test]
    fn discovery_helper_frame_rejects_invalid_headers_and_raw_layouts() {
        let raw = b"one bounded child transcript";
        let frame = encode_subagent_discovery_helper_success(discovery_wire_fixture(raw))
            .expect("encode discovery frame");
        let header_end = discovery_frame_header_end(&frame);

        let truncated_header = frame[..header_end - 1].to_vec();
        assert!(
            decode_discovery_wire_for_test(truncated_header).is_err(),
            "a truncated metadata header must fail closed"
        );

        let mut oversized_header = vec![0_u8; SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES];
        let oversized = u32::try_from(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP + 1)
            .expect("test header cap fits u32");
        oversized_header.copy_from_slice(&oversized.to_le_bytes());
        assert!(
            decode_discovery_wire_for_test(oversized_header).is_err(),
            "a metadata header over 64 KiB must fail closed"
        );

        let truncated_raw = frame[..frame.len() - 1].to_vec();
        assert!(
            decode_discovery_wire_for_test(truncated_raw).is_err(),
            "a truncated raw segment must fail closed"
        );

        let mut extra_raw = frame.clone();
        extra_raw.push(b'!');
        assert!(
            decode_discovery_wire_for_test(extra_raw).is_err(),
            "an extra raw tail must fail closed"
        );

        let response: SubagentDiscoveryHelperResponse = serde_json::from_slice(
            &frame[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end],
        )
        .expect("decode fixture metadata header");
        let mut mismatched_response = response;
        let SubagentDiscoveryHelperResponse::Ok { sources, .. } = &mut mismatched_response else {
            panic!("fixture must encode an OK response");
        };
        sources[0].byte_len = sources[0].byte_len.saturating_add(1);
        let mut mismatched_frame = encode_subagent_discovery_helper_header(&mismatched_response)
            .expect("encode mismatched metadata header");
        mismatched_frame.extend_from_slice(&frame[header_end..]);
        assert!(
            decode_discovery_wire_for_test(mismatched_frame).is_err(),
            "metadata byte lengths must exactly describe the raw tail"
        );
    }

    #[test]
    fn discovery_helper_error_frame_has_no_raw_tail_or_provider_text() {
        let marker = "provider-controlled-path-or-error-text";
        let frame = encode_subagent_discovery_helper_error(false)
            .expect("encode safe discovery helper failure frame");
        assert_eq!(
            discovery_frame_header_end(&frame),
            frame.len(),
            "a safe helper error must have no raw source tail"
        );
        assert!(
            !String::from_utf8_lossy(&frame).contains(marker),
            "a safe helper error frame must not contain provider-controlled text"
        );
        let error = decode_discovery_wire_for_test(frame)
            .expect_err("safe helper failure must not be accepted as discovery");
        assert!(
            error.to_string().contains("helper reported a safe failure"),
            "unexpected safe helper error: {error:#}"
        );
        assert!(
            !error.to_string().contains(marker),
            "parent error must not echo provider-controlled helper text: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_kills_a_stalled_test_helper_at_its_deadline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("stalled-subagent-discovery-helper");
        let pid_file = directory.path().join("stalled-subagent-discovery.pid");
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let started = Instant::now();
        let result = with_subagent_discovery_helper_program(helper, async {
            discover_claude_subagent_contents_bounded(
                directory.path(),
                "bounded-deadline-session",
                // Give the shell fixture enough startup headroom under a
                // parallel test build; the 30-second child still proves the
                // parent enforces its own bounded deadline and reaps it.
                Instant::now() + Duration::from_secs(2),
                1024,
                1,
            )
            .await
        })
        .await;

        assert!(
            result.is_err(),
            "a stalled helper must be killed instead of blocking child discovery"
        );
        assert!(
            format!("{:#}", result.expect_err("stalled helper must time out")).contains("deadline"),
            "the timeout must retain its typed deadline classification"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a killed helper must not outlive the bounded parent deadline"
        );

        let pid = async {
            for _ in 0..100 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("stalled helper must publish its PID before the deadline");
        let reaped = async {
            for _ in 0..50 {
                // SAFETY: signal zero probes the exact test child PID and
                // does not deliver a signal.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "deadline-killed discovery helper was not reaped");
    }

    /// The runtime's outer hook deadline cancels the discovery future before
    /// its own helper deadline. That cancellation must still kill and reap
    /// the direct helper rather than relying on Tokio's best-effort
    /// `kill_on_drop` behavior.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_outer_deadline_cancels_and_reaps_helper() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("outer-timeout-subagent-discovery-helper");
        let pid_file = directory
            .path()
            .join("outer-timeout-subagent-discovery.pid");
        write_stalled_helper(&helper, &pid_file);

        let (result, pid) = {
            let capture = with_subagent_discovery_helper_program(
                helper,
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "outer-timeout-discovery-session",
                    Instant::now() + Duration::from_secs(20),
                    1024,
                    1,
                ),
            );
            tokio::pin!(capture);
            let pid = tokio::select! {
                _ = &mut capture => panic!("stalled discovery helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&pid_file) => {
                    pid.expect("stalled discovery helper must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, pid)
        };
        assert!(
            result.is_err(),
            "the outer host deadline must cancel the still-live discovery helper"
        );
        assert!(
            wait_for_stalled_helper_reap(pid).await,
            "outer-cancelled discovery helper was not reaped"
        );
    }

    /// The discovery leader can exit after forking a child that retains both
    /// inherited pipes. The outer deadline must still kill the group while
    /// the parent intentionally waits for stdout EOF before reaping leader.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_outer_cancellation_kills_pipe_holding_descendant() {
        let directory = tempfile::tempdir().expect("create discovery tempdir");
        let helper = directory.path().join("forking-subagent-discovery-helper");
        let descendant_file = directory.path().join("discovery-descendant.pid");
        write_pipe_holding_descendant_helper(&helper, &descendant_file);

        let (result, descendant) = {
            let capture = with_subagent_discovery_helper_program(
                helper,
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "outer-timeout-discovery-session",
                    Instant::now() + Duration::from_secs(20),
                    1024,
                    1,
                ),
            );
            tokio::pin!(capture);
            let descendant = tokio::select! {
                _ = &mut capture => panic!("pipe-holding discovery helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&descendant_file) => {
                    pid.expect("pipe-holding discovery descendant must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, descendant)
        };
        assert!(
            result.is_err(),
            "outer deadline must cancel discovery with a pipe-holding descendant"
        );
        assert!(
            wait_for_stalled_helper_reap(descendant).await,
            "outer cancellation left a discovery pipe descendant alive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_rejects_stdout_beyond_its_read_cap() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("over-cap-subagent-discovery-helper");
        std::fs::write(
            &helper,
            b"#!/bin/sh\nexec /bin/dd if=/dev/zero bs=1024 count=2 2>/dev/null\n",
        )
        .expect("write over-cap discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let started = Instant::now();
        let result = with_subagent_discovery_helper_program(helper, async {
            with_subagent_discovery_helper_output_cap(1024, async {
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "bounded-over-cap-session",
                    Instant::now() + Duration::from_secs(2),
                    1024,
                    1,
                )
                .await
            })
            .await
        })
        .await;

        let error = result.expect_err("over-cap helper output must be rejected");
        assert!(
            error
                .to_string()
                .contains("helper returned an invalid response"),
            "unexpected over-cap helper error: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded stdout drain must not wait for all helper output"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_helper_failure_is_content_free() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("safe-failure-subagent-discovery-helper");
        let response = directory
            .path()
            .join("safe-failure-subagent-discovery-frame");
        std::fs::write(
            &response,
            encode_subagent_discovery_helper_error(false)
                .expect("encode safe discovery helper failure frame"),
        )
        .expect("write safe failure discovery frame");
        let response_path = response.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nexec /bin/cat '{response_path}'\n"),
        )
        .expect("write safe-failure discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let result = with_subagent_discovery_helper_program(helper, async {
            discover_claude_subagent_contents_bounded(
                directory.path(),
                "bounded-safe-failure-session",
                // This test exercises a safe error frame, not timeout
                // behavior. Leave scheduling headroom for a saturated full
                // suite so it does not turn into a spurious deadline result.
                Instant::now() + Duration::from_secs(5),
                1024,
                1,
            )
            .await
        })
        .await;
        let error = result.expect_err("helper error response must not be accepted");
        assert!(
            error.to_string().contains("helper reported a safe failure"),
            "unexpected helper failure: {error:#}"
        );
        assert!(
            !error.to_string().contains("bounded-safe-failure-session"),
            "helper failure must not echo provider-controlled values: {error:#}"
        );
    }

    #[test]
    fn projection_helper_returns_only_redacted_projection_bytes() {
        let secret = "sk-abcdefghijklmnopqrstuvwx123456";
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/redacted.jsonl",
            &child_transcript(secret),
            None,
        );
        let mut request = encode_subagent_projection_request_header(&source)
            .expect("encode projection request")
            .to_vec();
        request.extend_from_slice(&source.bytes);
        let frame =
            run_subagent_projection_helper(request).expect("run in-process projection helper");
        let projection = decode_subagent_projection_frame(
            frame,
            u64::try_from(source.bytes.len()).expect("fixture source length"),
        )
        .expect("decode safe projection helper frame");
        let projected = String::from_utf8_lossy(projection.transcript.as_ref());
        assert!(
            !projected.contains(secret),
            "raw child secret crossed the projection helper boundary: {projected}"
        );
        assert!(
            projection.turn_count > 0,
            "fixture must produce typed turns"
        );
        assert_eq!(
            hex::encode(projection.content_digest),
            subagent_content_identity_digest(
                &projection.transcript,
                projection.partial,
                source.malformed_lines,
            ),
            "the helper must bind the durable identity to its safe projection"
        );
    }

    #[test]
    fn short_valid_projection_uses_the_fixed_bounded_schema_floor() {
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/short.jsonl",
            &child_transcript("short typed child turn"),
            None,
        );
        let raw_bytes = u64::try_from(source.bytes.len()).expect("fixture source length");
        let transcript_cap = subagent_projection_transcript_cap(raw_bytes);
        assert_eq!(
            transcript_cap, SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP,
            "short JSONL sources must retain a fixed, bounded schema allowance"
        );
        let projection = safe_content_projection_bounded(&source, transcript_cap)
            .expect("short valid transcript must fit its bounded projection floor");
        assert!(
            u64::try_from(projection.transcript.len()).expect("projection length")
                <= transcript_cap,
            "projection must remain within the fixed short-source allowance"
        );
        assert!(
            subagent_projection_response_cap(raw_bytes) <= SUBAGENT_PROJECTION_HELPER_OUTPUT_CAP,
            "the fixed short-source allowance must remain within the helper's global response cap"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_deadline_kills_and_reaps_stalled_projection() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("stalled-subagent-projection-helper");
        let pid_file = directory.path().join("stalled-subagent-projection.pid");
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled projection helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/stalled.jsonl",
            &child_transcript("projection fixture"),
            None,
        );

        let started = Instant::now();
        let result = with_subagent_projection_helper_program(helper, async {
            safe_content_projection_until(&source, Instant::now() + Duration::from_secs(2)).await
        })
        .await;
        let error = result.expect_err("stalled projection helper must hit the deadline");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "stalled projection must preserve the capture deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "projection helper outlived its absolute deadline"
        );

        let pid = async {
            for _ in 0..50 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("stalled projection helper must publish its PID");
        let reaped = async {
            for _ in 0..50 {
                // SAFETY: signal zero probes the exact test child PID and
                // never delivers a signal.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "deadline-killed projection helper was not reaped");
    }

    /// A host capture deadline can cancel projection before this helper's own
    /// deadline. The cancellation guard must kill and reap it on that outer
    /// future-drop path too.
    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_outer_deadline_cancels_and_reaps_helper() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("outer-timeout-subagent-projection-helper");
        let pid_file = directory
            .path()
            .join("outer-timeout-subagent-projection.pid");
        write_stalled_helper(&helper, &pid_file);
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/outer-timeout.jsonl",
            &child_transcript("projection outer timeout fixture"),
            None,
        );

        let (result, pid) = {
            let capture = with_subagent_projection_helper_program(
                helper,
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(20)),
            );
            tokio::pin!(capture);
            let pid = tokio::select! {
                _ = &mut capture => panic!("stalled projection helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&pid_file) => {
                    pid.expect("stalled projection helper must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, pid)
        };
        assert!(
            result.is_err(),
            "the outer host deadline must cancel the still-live projection helper"
        );
        assert!(
            wait_for_stalled_helper_reap(pid).await,
            "outer-cancelled projection helper was not reaped"
        );
    }

    /// Projection receives raw child transcript bytes over stdin. A forked
    /// helper descendant retaining that fd and stdout must be group-killed on
    /// outer cancellation even after the direct leader has exited.
    #[cfg(unix)]
    #[tokio::test]
    async fn projection_outer_cancellation_kills_pipe_holding_descendant() {
        let directory = tempfile::tempdir().expect("create projection tempdir");
        let helper = directory.path().join("forking-subagent-projection-helper");
        let descendant_file = directory.path().join("projection-descendant.pid");
        write_pipe_holding_descendant_helper(&helper, &descendant_file);
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/outer-timeout.jsonl",
            &child_transcript("projection descendant fixture"),
            None,
        );

        let (result, descendant) = {
            let capture = with_subagent_projection_helper_program(
                helper,
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(20)),
            );
            tokio::pin!(capture);
            let descendant = tokio::select! {
                _ = &mut capture => panic!("pipe-holding projection helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&descendant_file) => {
                    pid.expect("pipe-holding projection descendant must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, descendant)
        };
        assert!(
            result.is_err(),
            "outer deadline must cancel projection with a pipe-holding descendant"
        );
        assert!(
            wait_for_stalled_helper_reap(descendant).await,
            "outer cancellation left a projection pipe descendant alive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_closes_stdin_before_waiting_for_eof() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("eof-subagent-projection-helper");
        let response = directory.path().join("eof-subagent-projection-response");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/eof.jsonl",
            &child_transcript("projection fixture"),
            None,
        );
        let mut request = encode_subagent_projection_request_header(&source)
            .expect("encode projection request")
            .to_vec();
        request.extend_from_slice(&source.bytes);
        std::fs::write(
            &response,
            run_subagent_projection_helper(request).expect("build projection helper response"),
        )
        .expect("write projection helper response");
        let response_path = response.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\n/bin/cat >/dev/null\nexec /bin/cat '{response_path}'\n"),
        )
        .expect("write EOF-sensitive projection helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let SafeSubagentProjection {
            partial,
            turn_count,
            ..
        } = with_subagent_projection_helper_program(helper, async {
            safe_content_projection_until(&source, Instant::now() + Duration::from_secs(5)).await
        })
        .await
        .expect("projection helper must receive EOF before the parent waits for it");
        assert!(!partial, "complete fixture must remain complete");
        assert!(
            turn_count > 0,
            "projection helper must return its response after EOF"
        );
    }

    #[tokio::test]
    async fn in_process_parent_validation_delay_respects_the_discovery_deadline() {
        let deadline = Instant::now() + Duration::from_millis(20);
        let result = with_subagent_parent_validation_delay(Duration::from_millis(80), async {
            subagent_discovery_test_pause_before_parent_validation(deadline).await;
            ensure_before_discovery_deadline(deadline)
        })
        .await;
        let error = result.expect_err("the in-process parent-validation delay must time out");
        assert!(
            format!("{error:#}").contains("deadline"),
            "the delay must retain the bounded-discovery error classification: {error:#}"
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
        // The V2 source capability and checkpoint object-index writer must
        // agree on one physical repository database. An in-memory connection
        // plus an empty on-disk marker lets the latter emit stranded repair
        // markers, which is not representative of a real capture.
        let database = storage_root.join(crate::utils::util::DATABASE);
        let conn = crate::internal::db::create_database(&database.to_string_lossy())
            .await
            .expect("repository database");
        ConfigKv::set_with_conn(&conn, "libra.repoid", "subagent-content-test", false)
            .await
            .expect("seed repository identity");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
        ))
        .await
        .expect("minimal ai_thread FK target");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('parent-session', 'claude_code', 'provider-session',
                       'active', ?, '{}', '{}', 1, 1, 1,
                       'subagent-content-test', '', NULL, NULL, 'scoped')",
            [directory.path().to_string_lossy().into_owned().into()],
        ))
        .await
        .expect("parent session");
        (directory, conn, storage_root)
    }

    async fn scoped_test_store() -> (
        tempfile::TempDir,
        DatabaseConnection,
        PathBuf,
        CaptureScope,
        String,
    ) {
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope {
            repo_id: "subagent-content-test".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("subagent-content-workspace".to_string()),
            workspace_fence: Some(9),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/subagent-content-workspace', 'agent',
                       'active', 'subagent-content-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live subagent workspace");
        let parent_session_id = "scoped-parent-session".to_string();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', 'scoped-provider-session',
                       'active', '/repo', '{}', '{}', 1, 1, 1,
                       ?, ?, ?, ?, 'scoped')",
            [
                parent_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed scoped subagent parent session");
        (directory, conn, storage_root, scope, parent_session_id)
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

    /// Direct SQL-sink tests bypass discovery, so they need the same opaque
    /// durable shape that the production capture boundary derives before
    /// calling reservation/finalization.
    fn durable_fixture(
        provider_kind: &str,
        transient_source_key: &str,
        bytes: &[u8],
        stable_subagent_id: Option<&str>,
    ) -> DiscoveredSubagentContent {
        let mut source = DiscoveredSubagentContent::fixture(
            provider_kind,
            transient_source_key,
            bytes,
            stable_subagent_id,
        );
        source.source_key = format!(
            "source/subagent-hmac-v2/{}",
            hex::encode(Sha256::digest(transient_source_key.as_bytes()))
        );
        source
    }

    async fn durable_source_for_test_capture(
        conn: &DatabaseConnection,
        storage_root: &Path,
        parent_root: &Path,
        source: &DiscoveredSubagentContent,
    ) -> DiscoveredSubagentContent {
        let scope = CaptureScope::main_for_connection(conn)
            .await
            .expect("test capture scope");
        let preimage = subagent_source_preimage(&source.provider_kind, &source.source_key, None)
            .expect("test source preimage");
        let commitment = derive_capture_source_commitment_in_scope_until(
            conn,
            &scope,
            storage_root,
            parent_root,
            CaptureSourceCommitmentDomain::SubagentSourceV2,
            &preimage,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("derive test V2 source commitment");
        let mut durable = source.clone();
        durable.legacy_source_key = Some(
            source_key_for_incarnation(&source.source_key, None).expect("test legacy source proof"),
        );
        durable.source_key = commitment;
        durable
    }

    #[tokio::test]
    async fn raw_subagent_locator_cannot_reach_reservation_sink() {
        let (_directory, conn, _storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/raw-locator.jsonl",
            &child_transcript("raw source must stay transient"),
            None,
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "test-digest",
                checkpoint_id: "test-checkpoint",
                owner: "test-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("raw subagent locator must be rejected before reservation");
        assert!(format!("{error:#}").contains("V2 source commitment"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim",
            )
            .await,
            0,
            "a rejected raw source must not create a durable claim"
        );
    }

    #[tokio::test]
    async fn legacy_sha_subagent_key_cannot_reach_reservation_sink() {
        let (_directory, conn, _storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "source/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            &child_transcript("legacy SHA source must stay read-only"),
            None,
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "test-digest",
                checkpoint_id: "test-checkpoint",
                owner: "test-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("legacy SHA source key must be rejected before reservation");
        assert!(format!("{error:#}").contains("V2 source commitment"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim",
            )
            .await,
            0,
            "a legacy SHA key must not create a durable claim"
        );
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

    fn test_mutation_deadline() -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_add(5_000),
        )
    }

    fn expired_test_mutation_deadline() -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() - Duration::from_millis(1),
            Utc::now().timestamp_millis().saturating_sub(1),
        )
    }

    /// A child writer can reserve its source before a workspace is released.
    /// Once that lease is stale, it must not create a marker, resolve a link,
    /// append child catalog rows, or clean up the reservation that a current
    /// owner may need to recover.
    #[tokio::test]
    async fn expired_workspace_scope_blocks_subagent_marker_link_final_and_release_mutations() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/scope-expiry.jsonl",
            &child_transcript("captured while workspace lease was live"),
            Some("scope-expiry-child"),
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-scope-expiry-checkpoint";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "scope-expiry-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("reserve child source while workspace lease is live");
        let ReservationOutcome::Reserved {
            fence_token,
            revision,
        } = reservation
        else {
            panic!("expected a live child reservation");
        };

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0 WHERE workspace_id = 'subagent-content-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after child reservation");

        let marker = TracesInflightMarker::new(&parent_session_id, checkpoint_id, 1);
        let marker_generation = marker
            .generation
            .as_deref()
            .expect("marker generation")
            .to_string();
        let error = register_traces_write_attempt_with_scope(
            &conn,
            &scope,
            &marker,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not register a child traces marker");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight' AND target = 'scoped-parent-session'
                   AND key = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "expired scope must not create a marker"
        );

        let error = refresh_current_link(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not refresh a child link");
        assert!(format!("{error:#}").contains("workspace lease"));

        let plan = ContentCommitPlan {
            preparation: None,
            capture_scope: scope.clone(),
            parent_session_id: parent_session_id.clone(),
            provider_kind: source.provider_kind.clone(),
            source_key: source.source_key.clone(),
            content_digest,
            checkpoint_id: checkpoint_id.to_string(),
            owner: "scope-expiry-owner".to_string(),
            fence_token,
            revision,
            parent_commit: None,
            source_channel: "live".to_string(),
            partial,
            stable_subagent_id: source.stable_subagent_id.clone(),
            codex_binding: None,
            created_at: 1,
            deadline: test_mutation_deadline(),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin child final transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "subagent-scope-expiry-commit".to_string(),
                    tree_oid: "subagent-scope-expiry-tree".to_string(),
                    metadata_blob_oid: "subagent-scope-expiry-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired scope must reject the final child transaction");
        assert!(format!("{error:#}").contains("workspace lease"));
        txn.rollback()
            .await
            .expect("roll back rejected child transaction");

        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                preparation: None,
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "scope-expiry-owner",
                fence_token,
            },
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not release a child reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "final child checkpoint row must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                 WHERE checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "child revision must not be appended"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE content_checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "child link must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'scope-expiry-owner'",
            )
            .await,
            1,
            "expired cleanup must leave the source reservation untouched"
        );
        let error = clear_non_cleanup_traces_inflight_marker_with_scope(
            &conn,
            &scope,
            &parent_session_id,
            checkpoint_id,
            &marker_generation,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not clear a child marker");
        assert!(format!("{error:#}").contains("workspace lease"));
    }

    /// Once the child slice expires, no new V2 durable claim, marker, link,
    /// checkpoint, or revision may appear. Releasing the exact reservation
    /// created before expiry is deliberately different: it is a bounded
    /// cleanup write that prevents the old owner from stranding a lease.
    #[tokio::test]
    async fn absolute_deadline_blocks_new_subagent_v2_writes_but_allows_owned_release() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/deadline.jsonl",
            &child_transcript("absolute deadline fixture"),
            Some("deadline-child"),
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-absolute-deadline-checkpoint";
        let expired = expired_test_mutation_deadline();

        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            expired,
        )
        .await
        .expect_err("expired capture must not create a V2 source claim");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "an expired capture must not create a V2 claim"
        );

        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before deadline expiry");
        let ReservationOutcome::Reserved {
            fence_token,
            revision,
        } = reservation
        else {
            panic!("expected provisional reservation");
        };

        let marker = TracesInflightMarker::new(&parent_session_id, checkpoint_id, 1);
        let error = register_traces_write_attempt_with_scope(&conn, &scope, &marker, expired)
            .await
            .expect_err("expired capture must not create a traces marker");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'
                   AND target = 'scoped-parent-session'
                   AND key = 'subagent-absolute-deadline-checkpoint'",
            )
            .await,
            0,
            "expired capture must not create a V2 traces marker"
        );

        let error = refresh_current_link(&conn, &scope, &parent_session_id, &source, expired)
            .await
            .expect_err("expired capture must not refresh a child link");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());

        let plan = ContentCommitPlan {
            preparation: None,
            capture_scope: scope.clone(),
            parent_session_id: parent_session_id.clone(),
            provider_kind: source.provider_kind.clone(),
            source_key: source.source_key.clone(),
            content_digest,
            checkpoint_id: checkpoint_id.to_string(),
            owner: "deadline-owner".to_string(),
            fence_token,
            revision,
            parent_commit: None,
            source_channel: "live".to_string(),
            partial,
            stable_subagent_id: source.stable_subagent_id.clone(),
            codex_binding: None,
            created_at: 1,
            deadline: expired,
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin expired finalization transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "subagent-absolute-deadline-commit".to_string(),
                    tree_oid: "subagent-absolute-deadline-tree".to_string(),
                    metadata_blob_oid: "subagent-absolute-deadline-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired capture must not finalize a child checkpoint");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        txn.rollback()
            .await
            .expect("roll back unused expired finalization transaction");

        for (table, predicate) in [
            (
                "agent_checkpoint",
                "checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
            (
                "agent_subagent_content_revision",
                "checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
            (
                "agent_subagent_link",
                "content_checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
        ] {
            let count = scalar(
                &conn,
                &format!("SELECT COUNT(*) AS n FROM {table} WHERE {predicate}"),
            )
            .await;
            assert_eq!(count, 0, "expired capture must not write {table}");
        }

        release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                preparation: None,
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "deadline-owner",
                fence_token,
            },
            expired,
        )
        .await
        .expect("expired capture may release its own provisional reservation");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'idle' AND owner IS NULL",
            )
            .await,
            1,
            "cleanup must retire only the existing owned reservation"
        );
    }

    /// A still-live process deadline may already be expired in SQLite's
    /// authoritative wall-clock domain. Recovery cleanup must retain that
    /// original fence rather than re-anchor it to a fresh 250ms grace after
    /// the reservation DML has run.
    #[tokio::test]
    async fn subagent_owned_release_rejects_expired_live_sqlite_deadline_at_final_authorization() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/release-final-sqlite-deadline.jsonl",
            &child_transcript("release final SQLite deadline fixture"),
            None,
        );
        let checkpoint_id = "subagent-release-final-sqlite-deadline-checkpoint";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "release-final-sqlite-deadline-digest",
                checkpoint_id,
                owner: "release-final-sqlite-deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before final-authorization deadline test");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                preparation: None,
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "release-final-sqlite-deadline-owner",
                fence_token,
            },
            deadline,
        )
        .await
        .expect_err("expired SQLite authorization must roll back a reservation release");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final authorization must preserve the capture-deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'release-final-sqlite-deadline-owner'",
            )
            .await,
            1,
            "an expired final SQLite deadline must leave the owned reservation intact"
        );
    }

    /// Recovery may release an existing reservation only during its short
    /// grace. A real independent SQLite `BEGIN EXCLUSIVE` lock must not keep
    /// the cancelled writer queued and release the reservation after the lock
    /// holder goes away.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_owned_release_recovery_grace_bounds_exclusive_sqlite_lock_without_late_write()
    {
        let (_directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/release-recovery-lock.jsonl",
            &child_transcript("release recovery lock fixture"),
            None,
        );
        let checkpoint_id = "subagent-release-recovery-lock-checkpoint";
        let owner = "release-recovery-lock-owner";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "release-recovery-lock-digest",
                checkpoint_id,
                owner,
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before recovery-lock test");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA journal_mode = DELETE".to_string(),
        ))
        .await
        .expect("force rollback-journal mode for release recovery-lock regression");
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent release recovery-lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent release recovery-lock contender");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "PRAGMA journal_mode = DELETE".to_string(),
            ))
            .await
            .expect("force rollback-journal mode on release recovery-lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive release recovery lock");

        let started = Instant::now();
        let release = tokio::time::timeout(
            Duration::from_secs(2),
            release_reservation(
                &contender,
                &scope,
                ReservationRelease {
                    preparation: None,
                    parent_session_id: &parent_session_id,
                    source: &source,
                    checkpoint_id,
                    owner,
                    fence_token,
                },
                expired_test_mutation_deadline(),
            ),
        )
        .await
        .expect("recovery release must respect its short writer-acquisition grace");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "ROLLBACK".to_string(),
            ))
            .await
            .expect("release exclusive recovery lock");

        let error = release.expect_err("exclusive lock must exhaust the recovery release grace");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "recovery writer timeout must preserve the capture-deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "recovery release must not wait for SQLite's default busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'release-recovery-lock-owner'",
            )
            .await,
            1,
            "a cancelled recovery writer wait must not release the reservation after lock release"
        );
    }

    /// The paired SQLite half is the authoritative last-write fence. A live
    /// process-side Instant with an already-expired SQLite deadline must roll
    /// back the claim written before final authorization, rather than merely
    /// rejecting at method entry.
    #[tokio::test]
    async fn subagent_claim_final_authorization_rejects_expired_sqlite_half_after_dml() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-sqlite-deadline.jsonl",
            &child_transcript("final SQLite deadline fixture"),
            None,
        );
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );

        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "final-sqlite-deadline-digest",
                checkpoint_id: "subagent-final-sqlite-deadline-checkpoint",
                owner: "final-sqlite-deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            deadline,
        )
        .await
        .expect_err("expired SQLite final authorization must roll back a new claim");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final authorization must retain the capture deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "failed final authorization must leave no newly durable V2 claim"
        );
    }

    /// The absolute deadline must also bound the SQLite writer wait itself.
    /// A separate connection holds the real file's writer slot; the contender
    /// may not wake up later and publish a V2 claim after its capture budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_v2_claim_deadline_cancels_contended_sqlite_writer_without_side_effect() {
        use sea_orm::TransactionTrait;

        let (_directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent SQLite lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent SQLite deadline contender");
        let held = holder.begin().await.expect("begin lock-holder transaction");
        held.execute_raw(Statement::from_sql_and_values(
            held.get_database_backend(),
            "INSERT INTO config_kv (key, value, encrypted) VALUES (?, 'hold', 0)",
            ["subagent-content-deadline-lock".into()],
        ))
        .await
        .expect("take SQLite writer lock");

        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/deadline-lock.jsonl",
            &child_transcript("deadline lock fixture"),
            None,
        );
        let result = reserve_source(
            &contender,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "deadline-lock-digest",
                checkpoint_id: "subagent-deadline-lock-checkpoint",
                owner: "deadline-lock-owner",
            },
            &UnchangedDurabilityProof::default(),
            CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_millis(30),
                Utc::now().timestamp_millis().saturating_add(30),
            ),
        )
        .await;
        held.rollback()
            .await
            .expect("release SQLite writer lock after deadline");
        let error = result.expect_err("contended reservation must stop at its deadline");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "writer-lock expiry must retain the subagent deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND content_schema_version = 2",
            )
            .await,
            0,
            "a canceled writer wait must not publish a delayed V2 claim"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_capture_deadline_bounds_initial_reads_under_exclusive_sqlite_lock() {
        let (directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA journal_mode = DELETE".to_string(),
        ))
        .await
        .expect("force rollback-journal mode for scoped capture deadline regression");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_session SET working_dir = ? WHERE session_id = ?",
            [
                directory.path().to_string_lossy().into_owned().into(),
                parent_session_id.clone().into(),
            ],
        ))
        .await
        .expect("point scoped parent at the test worktree");
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_lock_timeout(
            &database.to_string_lossy(),
            Duration::from_millis(50),
        )
        .await
        .expect("open scoped capture deadline lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "PRAGMA journal_mode = DELETE".to_string(),
            ))
            .await
            .expect("force rollback-journal mode on scoped capture deadline lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive scoped capture deadline lock");

        let sources = [DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/scoped-read-deadline.jsonl",
            &child_transcript("scoped read deadline fixture"),
            None,
        )];
        let started = Instant::now();
        let capture = tokio::time::timeout(
            Duration::from_secs(2),
            capture_discovered_subagent_contents_with_scope(
                &conn,
                &scope,
                &storage_root,
                &parent_session_id,
                &sources,
                "live",
                Some(CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_millis(30),
                    Utc::now().timestamp_millis().saturating_add(30),
                )),
            ),
        )
        .await
        .expect("scoped capture initial reads must honor their deadline");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "ROLLBACK".to_string(),
            ))
            .await
            .expect("release exclusive scoped capture deadline lock");

        let error = capture.expect_err("locked scoped capture must stop at its deadline");
        assert!(
            capture_deadline_exhausted(&error),
            "exclusive initial read lock must retain the subagent deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "scoped capture initial reads must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        for (sink, sql) in [
            (
                "subagent claim",
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            ),
            (
                "traces marker",
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight' AND target = 'scoped-parent-session'",
            ),
            (
                "checkpoint catalog row",
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE session_id = 'scoped-parent-session' AND scope = 'subagent'",
            ),
            (
                "traces ref",
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
            ),
        ] {
            assert_eq!(
                scalar(&conn, sql).await,
                0,
                "a cancelled scoped capture read must not publish a delayed {sink}"
            );
        }
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_subagent_reservation_and_release() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-scope-fence.jsonl",
            &child_transcript("final scope fence"),
            None,
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-final-scope-fence-checkpoint";

        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_claim_insert
             AFTER INSERT ON agent_subagent_content_claim
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install reservation scope-expiry trigger");
        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "final-scope-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects the initial reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "the expired lease rolls back the initial reservation"
        );
        conn.execute_unprepared("DROP TRIGGER expire_subagent_scope_after_claim_insert")
            .await
            .expect("remove reservation scope-expiry trigger");

        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "final-scope-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("reserve while the lease is live");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_claim_update
             AFTER UPDATE ON agent_subagent_content_claim
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install release scope-expiry trigger");
        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                preparation: None,
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "final-scope-owner",
                fence_token,
            },
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects the reservation release");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'final-scope-owner'",
            )
            .await,
            1,
            "the expired lease rolls back the reservation release"
        );
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_subagent_link_refresh() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-link-scope-fence.jsonl",
            &child_transcript("link final scope fence"),
            Some("stable-final-scope-child"),
        );
        let content_checkpoint_id = "subagent-final-scope-content";
        let boundary_checkpoint_id = "subagent-final-scope-boundary";
        for (checkpoint_id, stable_id) in [
            (content_checkpoint_id, None),
            (boundary_checkpoint_id, Some("stable-final-scope-child")),
        ] {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_checkpoint (
                    checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                    tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                    subagent_session_id, description, created_at
                 ) VALUES (?, ?, NULL, 'subagent', NULL, ?, ?, ?, NULL, ?,
                           'subagent scope-fence fixture', 1)",
                [
                    checkpoint_id.into(),
                    parent_session_id.clone().into(),
                    format!("tree-{checkpoint_id}").into(),
                    format!("metadata-{checkpoint_id}").into(),
                    format!("traces-{checkpoint_id}").into(),
                    stable_id.map(str::to_string).into(),
                ],
            ))
            .await
            .expect("seed subagent checkpoint fixture");
        }
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision_cursor, current_revision, current_checkpoint_id, current_digest,
                state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, sync_revision, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 1, 1, ?, 'digest', 'idle', NULL, NULL, NULL, NULL,
                       1, 1, 1, 1)",
            [
                parent_session_id.clone().into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                content_checkpoint_id.into(),
            ],
        ))
        .await
        .expect("seed current subagent content claim");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id,
                stable_subagent_id, sync_revision, created_at, updated_at
             ) VALUES (?, ?, 'unresolved', NULL, ?, 1, 1, 1)",
            [
                content_checkpoint_id.into(),
                parent_session_id.clone().into(),
                "stable-final-scope-child".into(),
            ],
        ))
        .await
        .expect("seed unresolved subagent link");
        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_link_update
             AFTER UPDATE ON agent_subagent_link
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install link scope-expiry trigger");

        let error = refresh_current_link(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects link refresh");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE content_checkpoint_id = 'subagent-final-scope-content'
                   AND link_state = 'unresolved' AND boundary_checkpoint_id IS NULL",
            )
            .await,
            1,
            "the expired lease rolls back the subagent link refresh"
        );
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
    #[serial(env)]
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
    #[serial(env)]
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
    #[serial(env)]
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
    #[serial(env)]
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
    async fn compatibility_wrapper_none_deadline_bounds_final_checkpoint_append() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/fallback-deadline.jsonl",
            &child_transcript("fallback deadline"),
            None,
        );
        let observed = Arc::new(std::sync::Mutex::new(None));
        let capture = TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE
            .scope(
                true,
                TEST_SUBAGENT_FINAL_APPEND_DEADLINE.scope(
                    Arc::clone(&observed),
                    capture_discovered_subagent_contents(
                        &conn,
                        &storage_root,
                        "parent-session",
                        &[source],
                        "live",
                        None,
                    ),
                ),
            )
            .await;
        let error = capture.expect_err(
            "the embedded unit-test host reaches the bounded append but has no private helper",
        );
        assert!(
            format!("{error:#}").contains("checkpoint object-I/O helper is unavailable"),
            "the regression must reach HistoryManager's deadline-bound append: {error:#}"
        );
        assert!(
            observed
                .lock()
                .expect("lock final append deadline observation")
                .expect("final checkpoint append must be reached")
                .is_some(),
            "a wrapper call with no caller deadline must pass its fallback deadline to history"
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

    /// The expired-reservation probe first clears only an existing provisional
    /// row, then validates the unchanged leaf. Its final authorization must
    /// still retain the paired deadline that was live at dispatch; otherwise a
    /// stale SQLite half would let that post-DML cleanup commit.
    #[tokio::test]
    async fn expired_reservation_probe_rolls_back_cleanup_after_expired_final_sqlite_deadline() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/expired-final-deadline.jsonl",
            &child_transcript("same final deadline"),
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
        .expect("seed an unchanged completed child before probe deadline test");

        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT source_key, current_digest FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("load durable source key and digest")
            .expect("seeded child claim");
        let source_key: String = row.try_get_by("source_key").expect("durable source key");
        let content_digest = row
            .try_get_by::<Option<String>, _>("current_digest")
            .expect("read completed child digest")
            .expect("completed child digest");
        let mut durable_source = source.clone();
        durable_source.source_key = source_key;
        durable_source.legacy_source_key = None;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'reserved', attempt_digest = current_digest,
                 attempt_checkpoint_id = 'expired-final-deadline-attempt',
                 owner = 'crashed-owner', lease_expires_at = ?, fence_token = 7",
            [Utc::now().timestamp_millis().saturating_sub(1).into()],
        ))
        .await
        .expect("seed expired matching reservation for deadline probe");

        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("resolve main capture scope");
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id: "expired-final-deadline-probe",
                owner: "probe-owner",
            },
            &UnchangedDurabilityProof::default(),
            deadline,
        )
        .await
        .expect_err("expired final SQLite deadline must roll back the cleanup probe");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final cleanup authorization must retain the deadline classification: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, owner, fence_token FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("query claim after rejected cleanup probe")
            .expect("seeded child claim remains");
        assert_eq!(
            row.try_get_by::<String, _>("state").expect("state"),
            "reserved"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner").expect("owner"),
            Some("crashed-owner".to_string())
        );
        assert_eq!(row.try_get_by::<i64, _>("fence_token").expect("fence"), 7);
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
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/crashed.jsonl",
            &child_transcript("final"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        let stale = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: "stale-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "crashed-writer",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
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
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/busy.jsonl",
            &child_transcript("final"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: "different-inflight-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "live-writer",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed live reservation");
        // The seeded lease is 60s. This budget has to outlast scope proof,
        // projection, and the first reservation probe under parallel nextest
        // load. A 30ms pair expired inside that work on compat-offline-core
        // (run 37915284178, three attempts) and the error never carried the
        // retryable writer text. Five seconds still ends while the lease is
        // held, so the wait stays a retry instead of a skip or a takeover.
        let deadline = Some(test_mutation_deadline());
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
        assert!(
            rendered.contains("another writer"),
            "live reservation must stay retryable, rendered: {rendered}"
        );
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
        let (directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/stale.jsonl",
            &child_transcript("stale-owner"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
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
                durable_source.provider_kind.clone().into(),
                durable_source.source_key.clone().into(),
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
            Arc::new(ClientStorage::try_init(storage_root.join("objects")).expect("storage")),
            storage_root.clone(),
            Arc::new(conn.clone()),
            crate::internal::branch::TRACES_BRANCH,
        );
        // Drain the fixture captures before arming the per-repository slow
        // consumer. This is test setup, not the production fix: old prune
        // rebuilding enqueued four *new* repair markers and then immediately
        // fenced its own marker batch at the test cap of three. Holding a
        // consumer makes that historical race deterministic.
        ClientStorage::wait_for_background_tasks();
        let _slow_repair_consumer = install_test_object_index_faults(
            &storage_root.join(crate::utils::util::DATABASE),
            ObjectIndexTestFaults {
                consumer_delay: Some(Duration::from_secs(5)),
                ..ObjectIndexTestFaults::default()
            },
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
        let rewritten = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT traces_commit, tree_oid FROM agent_checkpoint
                 WHERE scope = 'subagent' ORDER BY created_at ASC LIMIT 1"
                    .to_string(),
            ))
            .await
            .expect("read surviving rewritten checkpoint")
            .expect("surviving rewritten checkpoint");
        let rewritten_commit: String = rewritten
            .try_get_by("traces_commit")
            .expect("decode rewritten checkpoint commit");
        let rewritten_tree: String = rewritten
            .try_get_by("tree_oid")
            .expect("decode rewritten checkpoint tree");
        assert_eq!(
            scalar(
                &conn,
                &format!(
                    "SELECT COUNT(*) AS n FROM object_index
                     WHERE o_id IN ('{}', '{}')",
                    rewritten_commit, rewritten_tree
                ),
            )
            .await,
            2,
            "the prune transaction must index its reachable replacement commit and root tree"
        );
        let repair_marker_count = std::fs::read_dir(storage_root.join("object-index-repair"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .count();
        assert_eq!(
            repair_marker_count, 0,
            "prune rebuild must not publish repair markers that its deletion fence would self-observe"
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
            Arc::new(ClientStorage::try_init(storage_root.join("objects")).expect("storage")),
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
    #[cfg(test)]
    #[cfg(feature = "test-live-agent")]
    mod codex_child_writer_local_tests {
        use std::io::Read;

        use super::*;
        use crate::internal::ai::capture::catalog::CaptureCatalogPort;

        const CHILD: &str = "child-thread";
        const TURN: &str = "01991d45-6f00-7000-8000-000000000001";

        fn source() -> DiscoveredSubagentContent {
            let bytes = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{CHILD}\",\"session_id\":\"123e4567-e89b-12d3-a456-426614174031\",\"parent_thread_id\":\"parent-thread\"}}}}\n{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{TURN}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"WRITER_CHILD_THINKING_BODY\"}}],\"encrypted_content\":\"opaque-preserved\",\"future_field\":{{\"keep\":true}}}}}}\n"
            ).into_bytes();
            let mut source = DiscoveredSubagentContent::fixture(
                "codex",
                &codex_child_logical_source_key(
                    "123e4567-e89b-12d3-a456-426614174031",
                    "parent-thread",
                    CHILD,
                ),
                &bytes,
                Some(CHILD),
            );
            source.codex_binding = Some(CodexChildBinding {
                root: "123e4567-e89b-12d3-a456-426614174031".into(),
                parent: "parent-thread".into(),
                child: CHILD.into(),
                capture_session: "parent-session".into(),
                hook_turn: Some(TURN.into()),
            });
            source
        }

        async fn store() -> (tempfile::TempDir, DatabaseConnection, PathBuf) {
            let fixture = test_store().await;
            fixture.1.execute_raw(Statement::from_string(
                fixture.1.get_database_backend(),
                "UPDATE agent_session SET agent_kind = 'codex', provider_session_id = '123e4567-e89b-12d3-a456-426614174031'
                 WHERE session_id = 'parent-session'".to_string(),
            )).await.expect("owned Codex parent");
            fixture
        }

        fn candidate_usage_source() -> DiscoveredSubagentContent {
            let mut child = source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let mut rows = std::str::from_utf8(&child.bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            rows[0]["payload"]["parent_thread_id"] = serde_json::json!(root);
            for row in &mut rows {
                if row["payload"]["type"] == "reasoning" {
                    row["payload"]
                        .as_object_mut()
                        .unwrap()
                        .remove("encrypted_content");
                    row["payload"]
                        .as_object_mut()
                        .unwrap()
                        .remove("future_field");
                }
            }
            let usage = serde_json::json!({"input_tokens":3,"output_tokens":2,
                "cached_input_tokens":1,"cache_write_input_tokens":0,
                "reasoning_output_tokens":1,"total_tokens":5});
            rows.push(serde_json::json!({"type":"token_usage_record","payload":{
                "thread_id":CHILD,"session_id":root,"turn_id":TURN,"root_turn_id":TURN,
                "response_id":"candidate-response","usage":usage,
                "turn_token_usage":usage,"thread_token_usage":usage
            }}));
            child.bytes = (rows
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n")
                .into_bytes();
            child.codex_binding.as_mut().unwrap().parent = root.clone();
            child.source_key = codex_child_logical_source_key(&root, &root, CHILD);
            child
        }

        async fn candidate_attribution(
            conn: &DatabaseConnection,
            storage: &Path,
            summary: &SubagentCaptureSummary,
            thread: &str,
        ) -> serde_json::Value {
            candidate_attribution_with_deadline(
                conn,
                storage,
                summary,
                thread,
                Some(CaptureCommitDeadline::from_budget(Duration::from_secs(3)).unwrap()),
            )
            .await
        }

        async fn candidate_attribution_with_deadline(
            conn: &DatabaseConnection,
            storage: &Path,
            summary: &SubagentCaptureSummary,
            thread: &str,
            deadline: Option<CaptureCommitDeadline>,
        ) -> serde_json::Value {
            let scope = CaptureScope::main_for_connection(conn).await.unwrap();
            let cwd: String = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT working_dir FROM agent_session WHERE session_id='parent-session'"
                        .to_string(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("working_dir")
                .unwrap();
            let extraction = serde_json::json!({"present":true,"partial":false,
                "token_usage":{"input_tokens":10,"output_tokens":4,"cached_tokens":null,
                "reasoning_tokens":null,"total_tokens":14}});
            summary
                .project_usage_attribution(
                    conn,
                    storage,
                    SubagentUsageAttributionInput {
                        scope: &scope,
                        capture_session: "parent-session",
                        current_thread: thread,
                        verified_cwd: Path::new(&cwd),
                        agent_kind: "codex",
                        as_of: "live_capture",
                        parent_extraction: &extraction,
                        parent_snapshot_complete: true,
                        deadline,
                    },
                )
                .await
                .unwrap()
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_keeps_current_snapshot_and_rejects_old_revision() {
            let (_directory, conn, storage) = store().await;
            let mut child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let old = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            let before =
                candidate_metadata_bytes(&conn, &storage, &old.usage_candidates[0].checkpoint_id)
                    .await;
            let first = candidate_attribution(&conn, &storage, &old, &root).await;
            assert_eq!(first["source_count"], 1, "{first}");
            assert_eq!(first["unknown_source_count"], 0, "{first}");
            assert_eq!(first["sources"][0]["revision"], 1, "{first}");
            assert_eq!(
                first["sources"][0]["usage_provenance"],
                "computed_this_pass"
            );
            assert_eq!(first["child_token_usage"]["input_tokens"], 3);
            assert_eq!(first["parent_token_usage"]["input_tokens"], 10);
            // Exhaustive discovery proof and namespace difference classification are
            // separate prerequisites; one fresh leaf alone cannot prove completeness.
            assert_eq!(first["partial"], true);
            assert!(first["aggregate_token_usage"].is_null());
            child.bytes.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"CURRENCY_GROWTH\"}]}}\n");
            let grown = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            let stale = candidate_attribution(&conn, &storage, &old, &root).await;
            assert_eq!(stale["unknown_source_count"], 1, "{stale}");
            assert!(stale["sources"].as_array().unwrap().is_empty(), "{stale}");
            assert!(stale["child_token_usage"].is_null());
            let current = candidate_attribution(&conn, &storage, &grown, &root).await;
            assert_eq!(current["sources"][0]["revision"], 2, "{current}");
            assert_eq!(current["unknown_source_count"], 0);
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, &old.usage_candidates[0].checkpoint_id)
                    .await,
                before
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_rejects_forged_relationship_and_incarnation() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let original = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            for field in [
                "root",
                "parent",
                "child",
                "scope",
                "session",
                "incarnation",
                "digest",
                "checkpoint",
            ] {
                let mut forged = original.clone();
                let candidate = &mut forged.usage_candidates[0];
                match field {
                    "root" => candidate.root = "123e4567-e89b-12d3-a456-426614174099".into(),
                    "parent" => candidate.parent = "123e4567-e89b-12d3-a456-426614174098".into(),
                    "child" => candidate.child = "123e4567-e89b-12d3-a456-426614174097".into(),
                    "scope" => candidate.scope.worktree_id.push_str("-foreign"),
                    "session" => candidate.capture_session.push_str("-foreign"),
                    "incarnation" => candidate.capture_incarnation = Some("old-incarnation".into()),
                    "digest" => candidate.content_digest.push('0'),
                    "checkpoint" => candidate.checkpoint_id.push('0'),
                    _ => unreachable!(),
                }
                let value = candidate_attribution(&conn, &storage, &forged, &root).await;
                assert!(
                    value["sources"].as_array().unwrap().is_empty(),
                    "{field}: {value}"
                );
                assert_eq!(value["unknown_source_count"], 1, "{field}: {value}");
                assert!(value["aggregate_token_usage"].is_null());
            }
            conn.execute_raw(Statement::from_string(conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json=json_object('capture_incarnation','restored') WHERE session_id='parent-session'".to_string())).await.unwrap();
            let restored = candidate_attribution(&conn, &storage, &original, &root).await;
            assert!(
                restored["sources"].as_array().unwrap().is_empty(),
                "{restored}"
            );
            assert_eq!(restored["partial"], true);
            assert_eq!(restored["unknown_source_count"], 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_nested_import_uses_current_thread_not_root() {
            const NESTED: &str = "123e4567-e89b-12d3-a456-426614174092";
            const GRANDCHILD: &str = "123e4567-e89b-12d3-a456-426614174093";
            let (_directory, conn, storage) = store().await;
            let mut child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let mut rows = std::str::from_utf8(&child.bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            rows[0]["payload"]["id"] = GRANDCHILD.into();
            rows[0]["payload"]["parent_thread_id"] = NESTED.into();
            for row in &mut rows {
                if row["type"] == "token_usage_record" {
                    row["payload"]["thread_id"] = GRANDCHILD.into();
                }
            }
            child.bytes = (rows
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n")
                .into_bytes();
            let binding = child.codex_binding.as_mut().unwrap();
            binding.parent = NESTED.into();
            binding.child = GRANDCHILD.into();
            child.stable_subagent_id = Some(GRANDCHILD.into());
            child.source_key = codex_child_logical_source_key(&root, NESTED, GRANDCHILD);
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_session SET provider_session_id=? WHERE session_id='parent-session'",
                [NESTED.into()],
            ))
            .await
            .unwrap();
            let captured = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(captured.checkpoints_written, 1);
            let value = candidate_attribution(&conn, &storage, &captured, NESTED).await;
            assert_eq!(value["unknown_source_count"], 0, "{value}");
            assert!(value["sources"][0]["parent"].is_null(), "{value}");
            let native: serde_json::Value = serde_json::from_slice(
                &candidate_metadata_bytes(
                    &conn,
                    &storage,
                    &captured.usage_candidates[0].checkpoint_id,
                )
                .await,
            )
            .unwrap();
            assert_eq!(native["subagent"]["source_identity"]["parent"], NESTED);
            assert!(!value.to_string().contains(NESTED));
            assert_eq!(value["sources"][0]["depth"], 1);
            assert_eq!(value["child_token_usage"]["input_tokens"], 3);
            assert!(value["aggregate_token_usage"].is_null());
            // Independent transcript capture remains durable even though the
            // subtree's enumeration proof is not connected yet.
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope='subagent'"
                )
                .await,
                1
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_missing_companions_are_unknown_and_keep_body() {
            for table in ["agent_subagent_link", "agent_subagent_content_revision"] {
                let (_directory, conn, storage) = store().await;
                let child = candidate_usage_source();
                let root = child.codex_binding.as_ref().unwrap().root.clone();
                let captured = capture_discovered_subagent_contents(
                    &conn,
                    &storage,
                    "parent-session",
                    &[child],
                    "live",
                    None,
                )
                .await
                .unwrap();
                let checkpoint = &captured.usage_candidates[0].checkpoint_id;
                let before = candidate_metadata_bytes(&conn, &storage, checkpoint).await;
                conn.execute_raw(Statement::from_string(
                    conn.get_database_backend(),
                    format!("DELETE FROM {table}"),
                ))
                .await
                .unwrap();
                let value = candidate_attribution(&conn, &storage, &captured, &root).await;
                assert_eq!(value["unknown_source_count"], 1, "{table}: {value}");
                assert!(value["sources"].as_array().unwrap().is_empty());
                assert!(value["aggregate_token_usage"].is_null());
                assert_eq!(
                    candidate_metadata_bytes(&conn, &storage, checkpoint).await,
                    before
                );
            }
        }

        #[test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        fn codex_child_usage_currency_checked_subtotals_preserve_unknown_and_overflow() {
            let known = CodexUsageCounters {
                input_tokens: 3,
                output_tokens: 2,
                cached_tokens: Some(1),
                reasoning_tokens: Some(1),
                total_tokens: Some(5),
            };
            let mut unknown_optional = known.clone();
            unknown_optional.cached_tokens = None;
            let sum = sum_codex_usage_counters(&[known.clone(), unknown_optional]).unwrap();
            assert_eq!(sum.input_tokens, 6);
            assert_eq!(sum.cached_tokens, None);
            assert!(sum_codex_usage_counters(&[]).is_none());
            let huge = CodexUsageCounters {
                input_tokens: u64::MAX,
                output_tokens: 0,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(u64::MAX),
            };
            assert!(sum_codex_usage_counters(&[huge, known]).is_none());
            let value = serde_json::json!({"present":true,"partial":true,"token_usage":{
                "input_tokens":10,"output_tokens":4,"total_tokens":14}});
            let parent = CodexThreadUsage::from_parent_extraction(&value, true);
            assert_eq!(parent.usage_state, CodexUsageState::Partial);
            assert_eq!(parent.token_usage.unwrap().input_tokens, 10);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_absent_or_expired_deadline_preserves_parent_and_body() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let captured = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            let checkpoint = &captured.usage_candidates[0].checkpoint_id;
            let before = candidate_metadata_bytes(&conn, &storage, checkpoint).await;
            let expired = CaptureCommitDeadline::from_established_pair(
                Instant::now() - Duration::from_millis(1),
                Utc::now().timestamp_millis() - 1,
            );
            for deadline in [None, Some(expired)] {
                let value = candidate_attribution_with_deadline(
                    &conn, &storage, &captured, &root, deadline,
                )
                .await;
                assert_eq!(value["parent_token_usage"]["input_tokens"], 10, "{value}");
                assert_eq!(value["unknown_source_count"], 1, "{value}");
                assert!(value["child_token_usage"].is_null());
                assert!(value["aggregate_token_usage"].is_null());
                assert!(value["sources"].as_array().unwrap().is_empty());
                assert_eq!(value["partial"], true);
                assert_eq!(
                    candidate_metadata_bytes(&conn, &storage, checkpoint).await,
                    before
                );
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_descendant_references_require_current_ancestors() {
            const GRANDCHILD: &str = "123e4567-e89b-12d3-a456-426614174094";
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let mut grandchild = child.clone();
            let mut rows = std::str::from_utf8(&grandchild.bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            rows[0]["payload"]["id"] = GRANDCHILD.into();
            rows[0]["payload"]["parent_thread_id"] = CHILD.into();
            for row in &mut rows {
                if row["type"] == "token_usage_record" {
                    row["payload"]["thread_id"] = GRANDCHILD.into();
                }
            }
            grandchild.bytes = (rows
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n")
                .into_bytes();
            let binding = grandchild.codex_binding.as_mut().unwrap();
            binding.parent = CHILD.into();
            binding.child = GRANDCHILD.into();
            grandchild.stable_subagent_id = Some(GRANDCHILD.into());
            grandchild.source_key = codex_child_logical_source_key(&root, CHILD, GRANDCHILD);
            let captured = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child, grandchild],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(captured.checkpoints_written, 2);
            let value = candidate_attribution(&conn, &storage, &captured, &root).await;
            assert_eq!(value["unknown_source_count"], 0, "{value}");
            assert_eq!(value["child_token_usage"]["input_tokens"], 6);
            let sources = value["sources"].as_array().unwrap();
            assert_eq!(sources.len(), 2);
            let direct = sources.iter().find(|source| source["depth"] == 1).unwrap();
            let nested = sources.iter().find(|source| source["depth"] == 2).unwrap();
            assert!(direct["parent"].is_null());
            assert_eq!(nested["parent"], direct["source"]);
            for native in [root.as_str(), CHILD, GRANDCHILD] {
                assert!(!value.to_string().contains(native), "{native}: {value}");
            }
            let mut stale = captured.clone();
            stale
                .usage_candidates
                .iter_mut()
                .find(|candidate| candidate.child == CHILD)
                .unwrap()
                .checkpoint_id
                .push('0');
            let rejected = candidate_attribution(&conn, &storage, &stale, &root).await;
            assert_eq!(rejected["unknown_source_count"], 2, "{rejected}");
            assert!(rejected["sources"].as_array().unwrap().is_empty());
            assert!(rejected["child_token_usage"].is_null());
            assert!(rejected["aggregate_token_usage"].is_null());
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope='subagent'"
                )
                .await,
                2
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_missing_key_never_initializes_a_new_identity() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let captured = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            let checkpoint = &captured.usage_candidates[0].checkpoint_id;
            let before = candidate_metadata_bytes(&conn, &storage, checkpoint).await;
            let key = storage
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_DIR)
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_FILE);
            std::fs::remove_file(&key).unwrap();
            let value = candidate_attribution(&conn, &storage, &captured, &root).await;
            assert!(
                !key.exists(),
                "read-only usage attribution must not replace a missing capture key"
            );
            assert_eq!(value["unknown_source_count"], 1, "{value}");
            assert!(value["aggregate_token_usage"].is_null());
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, checkpoint).await,
                before
            );
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
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_currency_legacy_incarnation_requires_intact_metadata_role() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let root = child.codex_binding.as_ref().unwrap().root.clone();
            let old = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "import",
                None,
            )
            .await
            .unwrap();
            let old_id = &old.usage_candidates[0].checkpoint_id;
            let before = candidate_metadata_bytes(&conn, &storage, old_id).await;
            conn.execute_unprepared("UPDATE agent_session SET metadata_json=json_object('capture_incarnation','123e4567e89b42d3a456426614174199') WHERE session_id='parent-session'").await.unwrap();
            let current = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(current.checkpoints_written, 1);
            conn.execute_unprepared("UPDATE agent_session SET metadata_json=json_object('capture_incarnation','abcdefabcdefabcdefabcdefabcdefab') WHERE session_id='parent-session'").await.unwrap();
            let current = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[candidate_usage_source()],
                "import",
                None,
            )
            .await
            .unwrap();
            let value = candidate_attribution(&conn, &storage, &current, &root).await;
            assert_eq!(value["excluded_legacy_count"], 2, "{value}");
            assert_eq!(value["unknown_source_count"], 0, "{value}");
            assert_eq!(value["source_count"], 1, "{value}");
            assert_eq!(value["child_token_usage"]["input_tokens"], 3);
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, old_id).await,
                before
            );
            let (_, _, current_oid) =
                boundary_objects(&conn, &current.usage_candidates[0].checkpoint_id).await;
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_checkpoint SET metadata_blob_oid=? WHERE checkpoint_id=?",
                [current_oid.into(), old_id.clone().into()],
            ))
            .await
            .unwrap();
            let bad = candidate_attribution(&conn, &storage, &current, &root).await;
            assert_eq!(bad["excluded_legacy_count"], 1, "{bad}");
            assert_eq!(bad["unknown_source_count"], 1, "{bad}");
            assert!(bad["aggregate_token_usage"].is_null());
        }

        #[test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        fn codex_child_usage_parent_omission_does_not_disqualify_complete_parent() {
            let value = serde_json::json!({"present":true,"partial":true,
                "warnings":["deadline-bound extraction omitted child source aggregation"],
                "token_usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}});
            let parent = CodexThreadUsage::from_parent_extraction(&value, true);
            assert_eq!(parent.usage_state, CodexUsageState::Complete);
            assert_eq!(parent.token_usage.unwrap().input_tokens, 10);
            let mut uncertain = value.clone();
            uncertain["warnings"]
                .as_array_mut()
                .unwrap()
                .push("transcript parser reported partial output".into());
            assert_eq!(
                CodexThreadUsage::from_parent_extraction(&uncertain, true).usage_state,
                CodexUsageState::Partial
            );
            assert_eq!(
                CodexThreadUsage::from_parent_extraction(&value, false).usage_state,
                CodexUsageState::Partial
            );
        }

        async fn candidate_metadata_bytes(
            conn: &DatabaseConnection,
            storage: &Path,
            checkpoint: &str,
        ) -> Vec<u8> {
            let (_, _, oid) = boundary_objects(conn, checkpoint).await;
            let hash = crate::internal::object_format::parse_repo_oid(&oid).unwrap();
            let (bytes, truncated) =
                crate::internal::ai::checkpoint_reader::read_checkpoint_object_bounded(
                    storage,
                    &hash,
                    64 * 1024,
                )
                .unwrap();
            assert!(!truncated);
            bytes
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_new_replay_and_growth_keep_exact_durable_revision() {
            let (_directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
            let mut child = candidate_usage_source();
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(first.checkpoints_written, 1);
            assert_eq!(first.usage_candidates.len(), 1);
            assert!(!first.usage_candidates_incomplete);
            let candidate = &first.usage_candidates[0];
            assert_eq!(candidate.scope, scope);
            assert_eq!(candidate.capture_session, "parent-session");
            assert!(candidate.capture_incarnation.is_none());
            assert_eq!(candidate.root, child.codex_binding.as_ref().unwrap().root);
            assert_eq!(candidate.parent, candidate.root);
            assert_eq!(candidate.child, CHILD);
            assert_eq!(candidate.revision, 1);
            assert!(is_subagent_source_commitment_v2(&candidate.source_key));
            assert_eq!(candidate.usage.usage_state, CodexUsageState::Complete);
            assert_eq!(
                candidate.usage.token_usage.as_ref().unwrap().input_tokens,
                3
            );
            let before = candidate_metadata_bytes(&conn, &storage, &candidate.checkpoint_id).await;
            let replay = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(replay.checkpoints_written, 0);
            assert_eq!(replay.skipped_unchanged, 1);
            assert_eq!(replay.usage_candidates, first.usage_candidates);
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, &candidate.checkpoint_id).await,
                before
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                )
                .await,
                1
            );
            child.bytes.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"CANDIDATE_GROWTH\"}]}}\n");
            let grown = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(grown.usage_candidates.len(), 1);
            assert_eq!(grown.usage_candidates[0].revision, 2);
            assert_ne!(
                grown.usage_candidates[0].checkpoint_id,
                candidate.checkpoint_id
            );
            assert_ne!(
                grown.usage_candidates[0].content_digest,
                candidate.content_digest
            );
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, &candidate.checkpoint_id).await,
                before
            );
            let debug = format!("{first:?}");
            for private in [candidate.root.as_str(), CHILD, "WRITER_CHILD_THINKING_BODY"] {
                assert!(!debug.contains(private), "{debug}");
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_failure_preserves_only_already_durable_progress() {
            for (stage, committed) in [
                ("after_reservation", 0),
                ("after_marker", 0),
                ("after_first_commit", 1),
            ] {
                let (_directory, conn, storage) = store().await;
                let child = candidate_usage_source();
                let error = TEST_SUBAGENT_CONTENT_FAILPOINT
                    .scope(
                        Some(stage),
                        capture_discovered_subagent_contents(
                            &conn,
                            &storage,
                            "parent-session",
                            &[child],
                            "live",
                            None,
                        ),
                    )
                    .await
                    .unwrap_err();
                let progress = error
                    .downcast_ref::<SubagentCaptureProgressError>()
                    .unwrap()
                    .summary();
                assert_eq!(progress.checkpoints_written, committed, "{stage}");
                assert_eq!(progress.usage_candidates.len(), committed, "{stage}");
                assert!(progress.usage_candidates_incomplete, "{stage}");
                let forwarded = progress.clone().without_association_proofs();
                assert_eq!(forwarded.usage_candidates, progress.usage_candidates);
                assert!(forwarded.usage_candidates_incomplete);
                assert_eq!(
                    scalar(
                        &conn,
                        "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                    )
                    .await,
                    committed as i64
                );
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_unknown_still_retains_ordinary_child_checkpoint() {
            let (_directory, conn, storage) = store().await;
            let result = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source()],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.checkpoints_written, 1);
            assert_eq!(result.usage_candidates.len(), 1);
            let candidate = &result.usage_candidates[0];
            assert_eq!(candidate.usage, CodexThreadUsage::default());
            let body = candidate_metadata_bytes(&conn, &storage, &candidate.checkpoint_id).await;
            let metadata: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(metadata["subagent"]["usage"]["usage_state"], "unknown");
            assert_eq!(
                metadata["subagent"]["usage"]["token_usage"],
                serde_json::Value::Null
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_other_provider_has_no_codex_usage_observation() {
            let (_directory, conn, storage) = test_store().await;
            let child = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/usage-isolation.jsonl",
                &child_transcript("OTHER_PROVIDER_BODY"),
                None,
            );
            let result = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.checkpoints_written, 1);
            assert!(result.usage_candidates.is_empty());
            assert!(!result.usage_candidates_incomplete);
        }

        async fn seed_child_checkpoint_without_usage(
            conn: &DatabaseConnection,
            storage: &Path,
            raw: &DiscoveredSubagentContent,
        ) -> String {
            let scope = CaptureScope::main_for_connection(conn).await.unwrap();
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(10)).unwrap();
            // Use the legacy projection request and a real ref/catalog commit.
            // This fixture never rewrites a content-addressed object in place.
            let projection = safe_content_projection_until(raw, deadline.monotonic())
                .await
                .unwrap();
            assert!(projection.codex_usage.is_none());
            let mut owned = raw.clone();
            let cwd = storage.parent().unwrap().canonicalize().unwrap();
            let preimage = subagent_source_preimage("codex", &raw.source_key, None).unwrap();
            owned.source_key = derive_capture_source_commitment_in_scope_until(
                conn,
                &scope,
                storage,
                &cwd,
                CaptureSourceCommitmentDomain::SubagentSourceV2,
                &preimage,
                deadline.monotonic(),
            )
            .await
            .unwrap();
            let checkpoint = uuid::Uuid::new_v4().simple().to_string();
            let owner = uuid::Uuid::new_v4().to_string();
            let digest = hex::encode(projection.content_digest);
            let reserved = reserve_source(
                conn,
                &scope,
                "parent-session",
                &owned,
                ReservationAttempt {
                    content_digest: &digest,
                    checkpoint_id: &checkpoint,
                    owner: &owner,
                },
                &UnchangedDurabilityProof::default(),
                deadline,
            )
            .await
            .unwrap();
            let ReservationOutcome::Reserved {
                fence_token,
                revision,
            } = reserved
            else {
                panic!("fresh legacy fixture must reserve its source")
            };
            let now = Utc::now();
            let parent_commit = current_parent_commit(conn, Some(deadline.monotonic()))
                .await
                .unwrap();
            let binding = projection.codex_binding.as_ref().unwrap();
            let value = serde_json::json!({
                "schema_version":history::CHECKPOINT_METADATA_SCHEMA_VERSION,
                "checkpoint_id":checkpoint,"session_id":"parent-session","agent_kind":"codex",
                "scope":"subagent","created_at":now.timestamp(),
                "subagent":{"provenance":"content","source_key":owned.source_key,
                    "content_schema_version":SUBAGENT_CONTENT_SCHEMA_VERSION,
                    "revision":revision,"source_channel":"import","partial":projection.partial,
                    "source_identity":{"root":binding.root,"parent":binding.parent,"child":binding.child}}
            });
            assert!(value["subagent"].get("usage").is_none());
            let (metadata, _) =
                Redactor::new_default().redact(&serde_json::to_vec(&value).unwrap());
            let (report, _) = Redactor::new_default()
                .redact(&serde_json::to_vec(&projection.report_value).unwrap());
            let empty = RedactedBytes::new_unchecked(Vec::new());
            let marker =
                TracesInflightMarker::new("parent-session", &checkpoint, now.timestamp_millis());
            register_traces_write_attempt_with_scope(conn, &scope, &marker, deadline)
                .await
                .unwrap();
            let plan = ContentCommitPlan {
                preparation: None,
                capture_scope: scope.clone(),
                parent_session_id: "parent-session".into(),
                provider_kind: "codex".into(),
                source_key: owned.source_key,
                content_digest: digest,
                checkpoint_id: checkpoint.clone(),
                owner,
                fence_token,
                revision,
                parent_commit: parent_commit.clone(),
                source_channel: "import".into(),
                partial: projection.partial,
                stable_subagent_id: owned.stable_subagent_id,
                codex_binding: projection.codex_binding,
                created_at: now.timestamp(),
                deadline,
            };
            let manager = HistoryManager::new_with_ref(
                Arc::new(
                    ClientStorage::try_init(storage.join("objects"))
                        .expect("open the test object store"),
                ),
                storage.to_path_buf(),
                Arc::new(conn.clone()),
                crate::internal::branch::TRACES_BRANCH,
            );
            let written = manager
                .append_checkpoint_commit(CheckpointCommitParams {
                    reasoning_artifacts: &[],
                    checkpoint_id: &checkpoint,
                    session_id: "parent-session",
                    marker_generation: marker.generation.as_deref().unwrap(),
                    capture_scope: Some(&scope),
                    agent_kind: "codex",
                    parent_commit: parent_commit.as_deref(),
                    scope: CheckpointScope::Subagent,
                    tool_use_id: None,
                    metadata_json: &metadata,
                    transcript_redacted: &projection.transcript,
                    lifecycle_events_jsonl: &empty,
                    redaction_report_json: &report,
                    txn_extra: Some(&plan),
                    deadline: Some(deadline),
                })
                .await
                .unwrap();
            clear_non_cleanup_traces_inflight_marker_with_scope(
                conn,
                &scope,
                "parent-session",
                &checkpoint,
                &written.marker_generation,
                deadline,
            )
            .await
            .unwrap();
            checkpoint
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_legacy_metadata_absence_uses_current_pass_without_rewrite()
         {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let checkpoint =
                candidate_cli_helper(seed_child_checkpoint_without_usage(&conn, &storage, &child))
                    .await;
            let before = candidate_metadata_bytes(&conn, &storage, &checkpoint).await;
            let metadata: serde_json::Value = serde_json::from_slice(&before).unwrap();
            assert!(metadata["subagent"].get("usage").is_none());
            let result = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.checkpoints_written, 0);
            assert_eq!(result.skipped_unchanged, 1);
            assert_eq!(result.usage_candidates.len(), 1);
            let candidate = &result.usage_candidates[0];
            assert_eq!(candidate.checkpoint_id, checkpoint);
            assert_eq!(candidate.revision, 1);
            assert_eq!(candidate.usage.usage_state, CodexUsageState::Complete);
            assert_eq!(
                candidate.usage.token_usage.as_ref().unwrap().input_tokens,
                3
            );
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, &checkpoint).await,
                before
            );
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
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_same_batch_replay_counts_each_source_once() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let result = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child.clone(), child],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.checkpoints_written, 1);
            assert_eq!(result.skipped_unchanged, 1);
            assert_eq!(result.usage_candidates.len(), 1);
            assert_eq!(result.usage_candidates[0].revision, 1);
            assert!(!result.usage_candidates_incomplete);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_cap_keeps_all_ordinary_durable_progress() {
            let (_directory, conn, storage) = store().await;
            let sources = (0..=MAX_SUBAGENT_SOURCES_PER_CAPTURE)
                .map(|index| {
                    let mut child = candidate_usage_source();
                    let id = format!("usage-cap-child-{index}");
                    child.bytes = std::str::from_utf8(&child.bytes)
                        .unwrap()
                        .replace(CHILD, &id)
                        .into_bytes();
                    let binding = child.codex_binding.as_mut().unwrap();
                    binding.child = id.clone();
                    child.stable_subagent_id = Some(id);
                    child.source_key = codex_child_logical_source_key(
                        &binding.root,
                        &binding.parent,
                        &binding.child,
                    );
                    child
                })
                .collect::<Vec<_>>();
            // Compatibility callers can supply more bodies than discovery's
            // normal source cap. Optional usage must not discard those bodies.
            let result = candidate_cli_helper(capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &sources,
                "import",
                Some(CaptureCommitDeadline::from_budget(Duration::from_secs(20)).unwrap()),
            ))
            .await
            .unwrap();
            assert_eq!(
                result.checkpoints_written,
                MAX_SUBAGENT_SOURCES_PER_CAPTURE + 1
            );
            assert_eq!(
                result.usage_candidates.len(),
                MAX_SUBAGENT_SOURCES_PER_CAPTURE
            );
            assert!(result.usage_candidates_incomplete);
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                )
                .await,
                (MAX_SUBAGENT_SOURCES_PER_CAPTURE + 1) as i64
            );
        }

        async fn candidate_cli_helper<F: std::future::Future>(future: F) -> F::Output {
            let program = crate::utils::test::real_libra_helper_binary();
            assert!(
                program.is_file(),
                "build the current CLI before local deadline-bound candidate cases"
            );
            crate::internal::ai::authorized_read::with_test_helper_program(program, future).await
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_import_busy_marks_omitted_child_incomplete() {
            let (_directory, conn, storage) = store().await;
            let child = candidate_usage_source();
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(first.usage_candidates.len(), 1);
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_subagent_content_claim SET state='reserved', owner='another-owner',
                    attempt_digest=current_digest, attempt_checkpoint_id='another-checkpoint',
                    lease_expires_at=?, fence_token=fence_token+1",
                [(Utc::now().timestamp_millis() + 60_000).into()],
            ))
            .await
            .unwrap();
            let imported = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(imported.skipped_inflight, 1);
            assert!(imported.usage_candidates.is_empty());
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE owner='another-owner'").await, 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert!(
                imported.usage_candidates_incomplete,
                "a delivered but busy source is not complete-zero"
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_default_large_counters_survive_new_replay_and_duplicate()
         {
            let (_directory, conn, storage) = store().await;
            let mut child = candidate_usage_source();
            let mut rows = std::str::from_utf8(&child.bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            // Native Codex SDK counters are signed i64; the closed IPC DTO is u64.
            let usage = serde_json::json!({"input_tokens":i64::MAX-2,"output_tokens":2,
                "cached_input_tokens":1,"cache_write_input_tokens":0,
                "reasoning_output_tokens":1,"total_tokens":i64::MAX});
            for key in ["usage", "turn_token_usage", "thread_token_usage"] {
                rows.last_mut().unwrap()["payload"][key] = usage.clone();
            }
            child.bytes = (rows
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n")
                .into_bytes();
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            let expected = &first.usage_candidates[0];
            assert_eq!(expected.usage.usage_state, CodexUsageState::Complete);
            assert_eq!(
                expected.usage.token_usage.as_ref().unwrap().input_tokens,
                i64::MAX as u64 - 2
            );
            let before = candidate_metadata_bytes(&conn, &storage, &expected.checkpoint_id).await;
            let replay = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "import",
                None,
            )
            .await
            .unwrap();
            assert_eq!(replay.usage_candidates, first.usage_candidates);
            let duplicate = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child.clone(), child],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(duplicate.usage_candidates, first.usage_candidates);
            assert_eq!(duplicate.skipped_unchanged, 2);
            assert_eq!(
                candidate_metadata_bytes(&conn, &storage, &expected.checkpoint_id).await,
                before
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_candidate_out_of_sdk_range_is_unknown_without_losing_checkpoint()
        {
            let (_directory, conn, storage) = store().await;
            let mut child = candidate_usage_source();
            let mut rows = std::str::from_utf8(&child.bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            let invalid = serde_json::json!({"input_tokens":u64::MAX-2,"output_tokens":2,
                "cached_input_tokens":1,"cache_write_input_tokens":0,
                "reasoning_output_tokens":1,"total_tokens":u64::MAX});
            for key in ["usage", "turn_token_usage", "thread_token_usage"] {
                rows.last_mut().unwrap()["payload"][key] = invalid.clone();
            }
            child.bytes = (rows
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n")
                .into_bytes();
            let ordinary =
                safe_content_projection_until(&child, Instant::now() + Duration::from_secs(5))
                    .await
                    .unwrap();
            assert!(
                std::str::from_utf8(ordinary.transcript.as_ref())
                    .unwrap()
                    .contains("WRITER_CHILD_THINKING_BODY")
            );
            assert!(
                std::str::from_utf8(ordinary.transcript.as_ref())
                    .unwrap()
                    .contains(&u64::MAX.to_string())
            );
            let result = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.checkpoints_written, 1);
            assert_eq!(result.usage_candidates.len(), 1);
            let candidate = &result.usage_candidates[0];
            assert_eq!(candidate.usage, CodexThreadUsage::default());
            assert_eq!(
                candidate.content_digest,
                hex::encode(ordinary.content_digest)
            );
            let metadata =
                candidate_metadata_bytes(&conn, &storage, &candidate.checkpoint_id).await;
            let value: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
            assert_eq!(value["subagent"]["usage"]["usage_state"], "unknown");
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
        }

        const REPLAY_SESSION: &str = "codex__123e4567-e89b-12d3-a456-426614174031";

        use crate::internal::ai::observed_agents::live_capture::test_support::ScriptedChildDiscovery as ReplayDiscovery;

        fn set_replay_source(discovery: &ReplayDiscovery, source: DiscoveredSubagentContent) {
            discovery.set_result(SubagentDiscovery {
                bytes_read: source.bytes.len() as u64,
                sources: vec![source],
                ..SubagentDiscovery::default()
            });
        }

        async fn replay_store() -> (tempfile::TempDir, DatabaseConnection, PathBuf) {
            let fixture = store().await;
            let root = fixture.0.path().canonicalize().expect("owned worktree");
            fixture
                .1
                .execute_raw(Statement::from_sql_and_values(
                    fixture.1.get_database_backend(),
                    "UPDATE agent_session SET session_id = ?, working_dir = ?",
                    [
                        REPLAY_SESSION.into(),
                        root.to_string_lossy().into_owned().into(),
                    ],
                ))
                .await
                .expect("canonical fixture session");
            fixture
        }

        fn replay_source() -> DiscoveredSubagentContent {
            let mut child = source();
            child
                .codex_binding
                .as_mut()
                .expect("binding")
                .capture_session = REPLAY_SESSION.into();
            child
        }

        async fn replay_callback(
            directory: &Path,
            storage: &Path,
            conn: &DatabaseConnection,
            binding: crate::internal::ai::observed_agents::live_capture::LiveCaptureBinding,
            kind: crate::internal::ai::hooks::LifecycleEventKind,
            native_id: &str,
        ) -> Result<uuid::Uuid> {
            replay_callback_until(directory, storage, conn, binding, kind, native_id, None).await
        }

        async fn replay_callback_until(
            directory: &Path,
            storage: &Path,
            conn: &DatabaseConnection,
            binding: crate::internal::ai::observed_agents::live_capture::LiveCaptureBinding,
            kind: crate::internal::ai::hooks::LifecycleEventKind,
            native_id: &str,
            deadline: Option<crate::internal::ai::capture::ingress::CaptureDeadline>,
        ) -> Result<uuid::Uuid> {
            use crate::internal::ai::{
                capture::ingress::{
                    CaptureIngressBinding, CaptureIngressOutcome, CaptureRuntimeScope,
                    lower_child_hint_bound_frame_for_test,
                },
                hooks::{
                    LifecycleEventKind, provider::ProviderHookCommand, providers::codex_provider,
                },
            };
            let root = directory
                .canonicalize()
                .context("canonicalize owned root")?;
            let (name, command) = match kind {
                LifecycleEventKind::SubagentEnd => {
                    ("SubagentStop", ProviderHookCommand::SubagentEnd)
                }
                LifecycleEventKind::SessionStart => {
                    ("SessionStart", ProviderHookCommand::SessionStart)
                }
                LifecycleEventKind::SessionEnd => ("SessionEnd", ProviderHookCommand::SessionEnd),
                _ => panic!("unsupported local callback"),
            };
            let frame = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": name, "session_id": "123e4567-e89b-12d3-a456-426614174031", "cwd": root,
                "timestamp": "2026-10-20T16:00:00Z", "event_id": native_id,
                "agent_id": CHILD, "agent_transcript_path": root.join("child.jsonl"),
                "transcript_path": root.join("parent.jsonl"), "turn_id": TURN,
            }))?;
            let outcome = lower_child_hint_bound_frame_for_test(
                &frame,
                command,
                kind,
                codex_provider(),
                binding.child_source_hint_policy(),
                CaptureIngressBinding::new(
                    root.to_string_lossy().into_owned(),
                    [0xA5; 32],
                    CaptureRuntimeScope::new(storage.to_path_buf(), root),
                ),
                deadline,
            )?;
            let event = match &outcome {
                CaptureIngressOutcome::Command(command) => command.event_id(),
                _ => panic!("fixture callback must be recognized"),
            };
            // Deadline-bound publication uses the CLI that owns private argv,
            // never the unregistered libtest executable.
            let program = crate::utils::test::real_libra_helper_binary();
            anyhow::ensure!(
                program.is_file(),
                "build the current CLI with cargo build --features test-live-agent before running local child replay tests"
            );
            crate::internal::ai::authorized_read::with_test_helper_program(
                program,
                crate::internal::ai::capture::live_pipeline::ingest_agent_traces_ingress_outcome_for_test(
                    outcome, command, codex_provider(), binding, conn, Some(storage),
                ),
            ).await?;
            Ok(event)
        }

        async fn boundary_objects(conn: &DatabaseConnection, id: &str) -> (String, String, String) {
            let row = conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT traces_commit, tree_oid, metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
                [id.into()],
            )).await.expect("boundary query").expect("durable boundary");
            (
                row.try_get_by("traces_commit").expect("commit"),
                row.try_get_by("tree_oid").expect("tree"),
                row.try_get_by("metadata_blob_oid").expect("metadata"),
            )
        }

        async fn during_maintenance_contention<F, Fut, T>(
            storage: &Path,
            release_after: Option<Duration>,
            callback: F,
        ) -> (T, usize, Duration)
        where
            F: FnOnce() -> Fut + Send + 'static,
            Fut: std::future::Future<Output = T>,
            T: Send + 'static,
        {
            let storage = storage.to_path_buf();
            // Leave the outer runtime available to its SQLite pool while
            // an independent controller enforces the raw-lock watchdog.
            tokio::task::spawn_blocking(move || {
                use std::{
                    fs::OpenOptions,
                    sync::{
                        Arc,
                        atomic::{AtomicBool, AtomicUsize, Ordering},
                        mpsc,
                    },
                };

                // This descriptor deliberately bypasses the process registry:
                // a registered exclusive hold would exercise reentry, not waiting.
                let blocker = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(storage.join("maintenance.lock"))
                    .expect("owned maintenance file");
                blocker.lock().expect("raw exclusive maintenance hold");
                let releaser = release_after.map(|delay| {
                    let file = blocker.try_clone().expect("owned release descriptor");
                    std::thread::spawn(move || {
                        std::thread::sleep(delay);
                        file.unlock().expect("release owned raw blocker");
                    })
                });
                let (send, receive) = mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("local hook runtime");
                    let result = runtime.block_on(async move {
                        let done = Arc::new(AtomicBool::new(false));
                        let ticks = Arc::new(AtomicUsize::new(0));
                        let ticker = tokio::spawn({
                            let done = Arc::clone(&done);
                            let ticks = Arc::clone(&ticks);
                            async move {
                                while !done.load(Ordering::SeqCst) {
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    ticks.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                        });
                        let started = Instant::now();
                        let result = callback().await;
                        done.store(true, Ordering::SeqCst);
                        ticker.await.expect("ticker stopped");
                        (result, ticks.load(Ordering::SeqCst), started.elapsed())
                    });
                    let _ = send.send(result);
                });
                let received = receive.recv_timeout(Duration::from_secs(6));
                // A blocking regression must release its own blocker before
                // joining the worker; a Tokio timeout on that worker cannot do so.
                blocker.unlock().expect("watchdog releases raw blocker");
                if received.is_err() {
                    let cleanup = receive.recv_timeout(Duration::from_secs(6));
                    if matches!(cleanup, Err(mpsc::RecvTimeoutError::Timeout))
                        && !worker.is_finished()
                    {
                        std::process::abort();
                    }
                }
                worker.join().expect("owned callback thread reaped");
                if let Some(releaser) = releaser {
                    releaser.join().expect("owned release thread reaped");
                }
                received.expect("callback must finish while the raw blocker remains held")
            })
            .await
            .expect("owned contention controller reaped")
        }

        fn contended_replay_binding() -> (
            &'static ReplayDiscovery,
            crate::internal::ai::observed_agents::live_capture::LiveCaptureBinding,
        ) {
            use crate::internal::ai::{
                hooks::providers::codex_provider,
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let discovery: &'static ReplayDiscovery =
                Box::leak(Box::new(ReplayDiscovery::default()));
            set_replay_source(discovery, replay_source());
            (
                discovery,
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery),
            )
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_boundary_timeout_does_not_start_source_io() {
            use crate::internal::ai::{
                capture::ingress::CaptureDeadline, hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, ticks, elapsed) =
                during_maintenance_contention(&storage, None, move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SubagentEnd,
                        "maintenance-boundary",
                        Some(CaptureDeadline::from_budget_millis(800).unwrap()),
                    )
                    .await
                })
                .await;
            let error = result.expect_err("contended boundary cannot publish");
            assert!(format!("{error:#}").contains("maintenance lock deadline elapsed"));
            assert!(ticks > 5, "shared wait must yield the hook executor");
            assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
            assert_eq!(discovery.calls(), 0);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_session WHERE state != 'stopped' AND stopped_at IS NULL").await, 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_completed_replay_timeout_keeps_original_boundary() {
            use crate::internal::ai::{
                capture::ingress::CaptureDeadline, hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            discovery.set_result(SubagentDiscovery::default());
            let event = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "maintenance-replay",
            )
            .await
            .unwrap();
            let objects = boundary_objects(&conn, &boundary_id(event)).await;
            let head = traces_head(&conn, None).await.unwrap();
            set_replay_source(discovery, replay_source());
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, ticks, _) =
                during_maintenance_contention(&storage, None, move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SubagentEnd,
                        "maintenance-replay",
                        Some(CaptureDeadline::from_budget_millis(800).unwrap()),
                    )
                    .await
                })
                .await;
            let error = result.expect_err("replay must honor its lock deadline");
            assert!(format!("{error:#}").contains("maintenance lock deadline elapsed"));
            assert!(ticks > 5);
            assert_eq!(
                discovery.calls(),
                1,
                "only the initial empty boundary used discovery"
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(boundary_objects(&conn, &boundary_id(event)).await, objects);
            assert_eq!(traces_head(&conn, None).await.unwrap(), head);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_terminal_timeout_retains_pending_finalizer() {
            use crate::internal::ai::{
                capture::ingress::CaptureDeadline, hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, ticks, _) =
                during_maintenance_contention(&storage, None, move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SessionEnd,
                        "maintenance-terminal",
                        Some(CaptureDeadline::from_budget_millis(800).unwrap()),
                    )
                    .await
                })
                .await;
            let error = result.expect_err("terminal must remain pending on contention");
            assert!(format!("{error:#}").contains("maintenance lock deadline elapsed"));
            assert!(ticks > 5);
            assert_eq!(discovery.calls(), 0);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_session WHERE state != 'stopped' AND stopped_at IS NULL").await, 1);
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session".to_string(),
                ))
                .await
                .unwrap()
                .unwrap();
            let metadata: serde_json::Value =
                serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap())
                    .unwrap();
            let entries = metadata["capture_catalog_receipts_v1"]["entries"]
                .as_array()
                .unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["status"] == "pending" && entry["finalizer"].is_object())
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_released_lock_allows_real_content_and_boundary() {
            use crate::internal::ai::{
                capture::ingress::CaptureDeadline, hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, ticks, _) = during_maintenance_contention(
                &storage,
                Some(Duration::from_millis(100)),
                move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SubagentEnd,
                        "maintenance-released",
                        Some(CaptureDeadline::from_budget_millis(3000).unwrap()),
                    )
                    .await
                },
            )
            .await;
            let event =
                result.expect("released guard permits publication before original deadline");
            assert!(ticks > 0);
            assert_eq!(discovery.calls(), 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                1
            );
            let _ = boundary_objects(&conn, &boundary_id(event)).await;
            assert_stored_child_body(&storage, &replay_source().bytes);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_wait_does_not_extend_original_commit_deadline() {
            use crate::internal::ai::{
                capture::ingress::CaptureDeadline, hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            // Deliberately misbehaving scripted discovery returns after the
            // host deadline. The real commit path must still reject it.
            discovery.before_next(Box::pin(async {
                tokio::time::sleep(Duration::from_millis(900)).await;
                Ok(())
            }));
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, _, _) = during_maintenance_contention(
                &storage,
                Some(Duration::from_millis(100)),
                move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SubagentEnd,
                        "maintenance-original-deadline",
                        Some(CaptureDeadline::from_budget_millis(800).unwrap()),
                    )
                    .await
                },
            )
            .await;
            let error = result.expect_err("source delay cannot grant a fresh commit budget");
            assert!(format!("{error:#}").contains("deadline"), "{error:#}");
            assert!(!format!("{error:#}").contains("maintenance lock deadline"));
            assert_eq!(discovery.calls(), 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_maintenance_child_cap_keeps_host_window_for_partial_boundary() {
            use crate::internal::ai::{
                capture::{
                    ingress::CaptureDeadline, live_pipeline::with_child_capture_budget_for_test,
                },
                hooks::LifecycleEventKind,
            };
            let (directory, conn, storage) = replay_store().await;
            let (discovery, binding) = contended_replay_binding();
            // Keep the local cap below the scripted source delay, with enough
            // host slack for helper cold starts when the whole family runs.
            discovery.before_next(Box::pin(async {
                tokio::time::sleep(Duration::from_millis(750)).await;
                Ok(())
            }));
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (result, _, elapsed) = during_maintenance_contention(
                &storage,
                Some(Duration::from_millis(50)),
                move || async move {
                    with_child_capture_budget_for_test(
                        500,
                        replay_callback_until(
                            &root,
                            &held_storage,
                            &database,
                            binding,
                            LifecycleEventKind::SubagentEnd,
                            "maintenance-child-cap",
                            Some(CaptureDeadline::from_budget_millis(5000).unwrap()),
                        ),
                    )
                    .await
                },
            )
            .await;
            let event = result.expect("original host window permits a partial durable boundary");
            // The scripted port deliberately returns late; the cap is checked
            // cooperatively before content publication, not by preempting it.
            assert!(elapsed >= Duration::from_millis(750));
            assert_eq!(discovery.calls(), 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                0
            );
            let objects = boundary_objects(&conn, &boundary_id(event)).await;
            let metadata: serde_json::Value =
                serde_json::from_slice(&blob(&storage, &objects.2)).unwrap();
            assert_eq!(metadata["subagent"]["content_capture"]["state"], "partial");
            assert!(metadata["subagent_session_id"].is_null());

            // The same boundary can later collect the actual child after a
            // contended lock is released; its original objects stay immutable.
            let root = directory.path().to_path_buf();
            let database = conn.clone();
            let held_storage = storage.clone();
            let (replayed, _, _) = during_maintenance_contention(
                &storage,
                Some(Duration::from_millis(50)),
                move || async move {
                    replay_callback_until(
                        &root,
                        &held_storage,
                        &database,
                        binding,
                        LifecycleEventKind::SubagentEnd,
                        "maintenance-child-cap",
                        Some(CaptureDeadline::from_budget_millis(5000).unwrap()),
                    )
                    .await
                },
            )
            .await;
            assert_eq!(replayed.expect("bounded late replay after release"), event);
            assert_eq!(discovery.calls(), 2);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                1
            );
            assert_eq!(boundary_objects(&conn, &boundary_id(event)).await, objects);
            assert_stored_child_body(&storage, &replay_source().bytes);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_replay_completed_boundary_collects_late_content_once() {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, providers::codex_provider},
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let (directory, conn, storage) = replay_store().await;
            let discovery = Box::leak(Box::new(ReplayDiscovery::default()));
            let binding =
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery);
            let event = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "same-late-stop",
            )
            .await
            .expect("initial empty boundary");
            let id = boundary_id(event);
            let immutable = boundary_objects(&conn, &id).await;
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            set_replay_source(discovery, replay_source());
            let replay = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "same-late-stop",
            )
            .await
            .expect("late content replay");
            assert_eq!(event, replay);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved' AND sync_revision = 2").await, 1);
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT boundary_checkpoint_id FROM agent_subagent_link".to_string(),
                ))
                .await
                .expect("link")
                .expect("child");
            assert_eq!(
                row.try_get_by::<String, _>("boundary_checkpoint_id")
                    .expect("boundary"),
                id
            );
            let head = traces_head(&conn, None).await.expect("head");
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "same-late-stop",
            )
            .await
            .expect("unchanged replay");
            assert_eq!(traces_head(&conn, None).await.expect("head"), head);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE sync_revision = 2"
                )
                .await,
                1
            );
            assert_eq!(boundary_objects(&conn, &id).await, immutable);
            assert_eq!(discovery.calls(), 3);
            let mut grown = replay_source();
            grown.bytes.extend_from_slice(br#"{"type":"response_item","payload":{"type":"reasoning","text":"EXTRA_LATE_THINKING"}}
"#);
            set_replay_source(discovery, grown);
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "same-late-stop",
            )
            .await
            .expect("new source revision");
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                3
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE current_revision = 2").await, 1);
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                2
            );
            assert_eq!(
                boundary_objects(&conn, &id).await,
                immutable,
                "new content revisions must not rewrite the original boundary"
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_replay_pending_receipt_existing_checkpoint_consumes_fresh_proof() {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, providers::codex_provider},
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let (directory, conn, storage) = replay_store().await;
            let discovery = Box::leak(Box::new(ReplayDiscovery::default()));
            let binding =
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery);
            let event = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "pending-existing-stop",
            )
            .await
            .expect("initial boundary");
            let id = boundary_id(event);
            let immutable = boundary_objects(&conn, &id).await;
            // Model the real ref/catalog -> receipt-completion crash window.
            conn.execute_raw(Statement::from_string(conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json = json_set(metadata_json, '$.capture_catalog_receipts_v1.entries[0].status', 'pending')".to_string(),
            )).await.expect("pending crash fixture");
            set_replay_source(discovery, replay_source());
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "pending-existing-stop",
            )
            .await
            .expect("facade existing replay");
            assert_eq!(boundary_objects(&conn, &id).await, immutable);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                1
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_session WHERE json_extract(metadata_json, '$.capture_catalog_receipts_v1.entries[0].status') = 'complete'").await, 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_replay_new_session_start_supersedes_old_boundary_before_io() {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, providers::codex_provider},
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let (directory, conn, storage) = replay_store().await;
            let discovery = Box::leak(Box::new(ReplayDiscovery::default()));
            let binding =
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery);
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "old-stop",
            )
            .await
            .expect("old boundary");
            set_replay_source(discovery, replay_source());
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SessionStart,
                "new-session-start",
            )
            .await
            .expect("new lifecycle action");
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "old-stop",
            )
            .await
            .expect("old callback no-op");
            assert_eq!(
                discovery.calls(),
                1,
                "superseded receipt must not start provider IO"
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link").await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_replay_new_action_during_discovery_keeps_content_unresolved() {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, providers::codex_provider},
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let (directory, conn, storage) = replay_store().await;
            let discovery = Box::leak(Box::new(ReplayDiscovery::default()));
            let binding =
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery);
            replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "racing-stop",
            )
            .await
            .expect("initial boundary");
            set_replay_source(discovery, replay_source());
            let concurrent = conn.clone();
            discovery.before_next(Box::pin(async move {
                concurrent
                    .execute_raw(Statement::from_string(
                        concurrent.get_database_backend(),
                        "UPDATE agent_session SET sync_revision = sync_revision + 1".to_string(),
                    ))
                    .await
                    .context("simulate a concurrent lifecycle mutation")?;
                Ok(())
            }));
            let error = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "racing-stop",
            )
            .await
            .expect_err("new action fences replay");
            assert!(format!("{error:#}").contains("superseded"));
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2,
                "source already obtained before the fence changed must remain durable"
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL").await, 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_replay_damaged_boundary_keeps_new_content_without_association() {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, providers::codex_provider},
                observed_agents::live_capture::{
                    LiveCaptureBinding, test_support::with_subagent_discovery,
                },
            };
            let (directory, conn, storage) = replay_store().await;
            let discovery = Box::leak(Box::new(ReplayDiscovery::default()));
            let binding =
                with_subagent_discovery(LiveCaptureBinding::resolve(codex_provider()), discovery);
            let event = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "damaged-boundary-stop",
            )
            .await
            .expect("initial boundary");
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_checkpoint SET metadata_blob_oid = ? WHERE checkpoint_id = ?",
                ["0".repeat(40).into(), boundary_id(event).into()],
            ))
            .await
            .expect("damage only the owned boundary fixture");
            set_replay_source(discovery, replay_source());
            let error = replay_callback(
                directory.path(),
                &storage,
                &conn,
                binding,
                LifecycleEventKind::SubagentEnd,
                "damaged-boundary-stop",
            )
            .await
            .expect_err("unproved boundary objects");
            assert!(format!("{error:#}").contains("verify existing child boundary objects"));
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2,
                "old boundary damage must not delete newly obtained thinking"
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL").await, 1);
        }

        fn blob(storage: &Path, oid: &str) -> Vec<u8> {
            let bytes = std::fs::read(storage.join("objects").join(&oid[..2]).join(&oid[2..]))
                .expect("owned loose object");
            let mut decoded = Vec::new();
            flate2::read::ZlibDecoder::new(bytes.as_slice())
                .read_to_end(&mut decoded)
                .expect("decode owned loose object");
            let end = decoded.iter().position(|b| *b == 0).expect("object header");
            assert!(decoded[..end].starts_with(b"blob "));
            decoded[end + 1..].to_vec()
        }

        fn assert_stored_child_body(storage: &Path, expected: &[u8]) {
            for prefix in std::fs::read_dir(storage.join("objects")).expect("owned objects") {
                let prefix = prefix.expect("object prefix");
                if prefix.file_name().to_string_lossy().len() != 2 || !prefix.path().is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(prefix.path()).expect("owned object prefix") {
                    let bytes =
                        std::fs::read(entry.expect("owned object").path()).expect("object bytes");
                    let mut decoded = Vec::new();
                    flate2::read::ZlibDecoder::new(bytes.as_slice())
                        .read_to_end(&mut decoded)
                        .expect("loose object");
                    if let Some(end) = decoded.iter().position(|b| *b == 0)
                        && decoded[..end].starts_with(b"blob ")
                        && &decoded[end + 1..] == expected
                    {
                        return;
                    }
                }
            }
            panic!(
                "the actual object store must contain the exact readable thinking and opaque/unknown bytes"
            );
        }

        async fn prepare_parent_request(conn: &DatabaseConnection) -> CaptureCatalogApplyRequest {
            use crate::internal::ai::capture::{
                catalog::{
                    CaptureCatalogAction, CaptureCatalogMutation, CaptureCatalogSession,
                    CaptureCatalogStore,
                },
                state::{
                    CapturePhase, DurableCaptureState, LifecycleReducerInput, reduce_lifecycle,
                },
            };
            let row = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
                "SELECT state, stopped_at, sync_revision, working_dir FROM agent_session WHERE session_id='parent-session'".to_string())).await.unwrap().unwrap();
            let state = DurableCaptureState {
                phase: CapturePhase::from_db(&row.try_get_by::<String, _>("state").unwrap())
                    .unwrap(),
                stopped_at: row.try_get_by("stopped_at").unwrap(),
                sync_revision: row.try_get_by("sync_revision").unwrap(),
            };
            let event_id = uuid::Uuid::from_u128(0xc301);
            let kind = crate::internal::ai::hooks::LifecycleEventKind::SessionEnd;
            let plan = reduce_lifecycle(LifecycleReducerInput {
                current: Some(state),
                event_kind: kind,
                event_id,
                occurred_at: 2,
                deadline: None,
            })
            .unwrap();
            let request = CaptureCatalogApplyRequest::new(
                CaptureScope::main_for_connection(conn).await.unwrap(),
                CaptureCatalogSession::new(
                    "parent-session",
                    "codex",
                    "123e4567-e89b-12d3-a456-426614174031",
                    row.try_get_by::<String, _>("working_dir").unwrap(),
                )
                .unwrap(),
                CaptureCatalogAction::from_ingress(event_id, None, kind).unwrap(),
                CaptureCatalogMutation::from_reducer(Some(state), &plan, 2).unwrap(),
            )
            .unwrap()
            .requiring_child_checkpoints()
            .unwrap();
            CaptureCatalogStore::new(conn.clone())
                .apply(&request)
                .await
                .unwrap();
            request
        }

        async fn recovery_token(
            conn: &DatabaseConnection,
            request: &CaptureCatalogApplyRequest,
        ) -> CaptureCatalogFinalizerRecovery {
            use crate::internal::ai::capture::{
                catalog::{CaptureCatalogFinalizeRequest, CaptureCatalogStore},
                finalizer::{
                    CaptureFinalizeMode, CaptureFinalizePolicy, FinalizeCheckpointProgress,
                },
            };
            let policy = CaptureFinalizePolicy::new(
                Some(10),
                CaptureFinalizeMode::Deferrable,
                request.action().action_key(),
            )
            .unwrap();
            let finalize = CaptureCatalogFinalizeRequest::from_apply(
                request,
                policy,
                uuid::Uuid::new_v4().to_string(),
                None,
                1,
                FinalizeCheckpointProgress::NotStarted,
            )
            .unwrap();
            let store = CaptureCatalogStore::new(conn.clone());
            store.finalize(&finalize).await.unwrap();
            let mut scan = store
                .pending_finalizer_recoveries_for_doctor(
                    2 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS,
                )
                .await
                .unwrap();
            assert_eq!(scan.recoveries.len(), 1);
            let recovery = scan.recoveries.remove(0);
            assert!(
                recovery.budget_exhausted(),
                "test must preserve an actually exhausted original policy"
            );
            recovery
        }

        async fn recover_with_port(
            conn: &DatabaseConnection,
            storage: &Path,
            recovery: &CaptureCatalogFinalizerRecovery,
            port: &'static ReplayDiscovery,
            deadline: CaptureCommitDeadline,
        ) -> Result<()> {
            use crate::internal::ai::observed_agents::live_capture::{
                LiveCaptureBinding, test_support::with_subagent_discovery,
            };
            let program = crate::utils::test::real_libra_helper_binary();
            anyhow::ensure!(
                program.is_file(),
                "build current CLI before local recovery tests"
            );
            crate::internal::ai::authorized_read::with_test_helper_program(
                program,
                recover_terminal_children_with_binding(conn, storage, recovery, deadline, |kind| {
                    with_subagent_discovery(LiveCaptureBinding::for_stored(kind), port)
                }),
            )
            .await
        }

        fn recovered_source(directory: &Path) -> DiscoveredSubagentContent {
            let mut child = source();
            let end = child.bytes.iter().position(|byte| *byte == b'\n').unwrap();
            let mut header: serde_json::Value =
                serde_json::from_slice(&child.bytes[..end]).unwrap();
            header["payload"]["cwd"] = serde_json::json!(directory.canonicalize().unwrap());
            let mut bytes = serde_json::to_vec(&header).unwrap();
            bytes.push(b'\n');
            bytes.extend_from_slice(&child.bytes[end + 1..]);
            child.bytes = bytes;
            child.codex_binding.as_mut().unwrap().hook_turn = None;
            child
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_materializes_body_original_floors_and_settled_zero_io() {
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let child = recovered_source(dir.path());
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            set_replay_source(port, child.clone());
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap();
            recover_with_port(&conn, &storage, &recovery, port, deadline)
                .await
                .unwrap();
            assert_stored_child_body(&storage, &child.bytes);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_eq!(
                parent_gate(&conn).await["requirements"][0]["minimum_revision"],
                1
            );
            let calls = port.stored_calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].libra_session_id, "parent-session");
            assert_eq!(
                calls[0].provider_session_id,
                "123e4567-e89b-12d3-a456-426614174031"
            );
            assert_eq!(calls[0].cwd, dir.path());
            assert_eq!(calls[0].byte_budget, TRANSCRIPT_READ_HARD_CAP_BYTES);
            assert_eq!(calls[0].source_limit, 16);
            assert_eq!(calls[0].deadline, deadline.monotonic());
            assert!(calls[0].recovery_root);
            recover_terminal_children_with_binding(
                &conn,
                &storage,
                &recovery,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
                |_| panic!("settled leaves must not even select a provider"),
            )
            .await
            .unwrap();
            assert_eq!(port.calls(), 1);
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                )
                .await,
                1
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE boundary_checkpoint_id IS NOT NULL").await, 0);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_bounded_partial_keeps_body_and_finishes_observed_floors() {
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let child = recovered_source(dir.path());
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            port.set_result(SubagentDiscovery {
                sources: vec![child.clone()],
                bytes_read: child.bytes.len() as u64,
                incomplete: true,
                ..Default::default()
            });
            recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_stored_child_body(&storage, &child.bytes);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            recover_terminal_children_with_binding(
                &conn,
                &storage,
                &recovery,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
                |_| panic!("a bounded partial pass cannot demand a complete rescan"),
            )
            .await
            .unwrap();
            assert_eq!(port.calls(), 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_unavailable_partial_retains_body_floor_and_preparing() {
            use crate::internal::ai::observed_agents::live_capture::SubagentDiscoveryAvailability;
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let child = recovered_source(dir.path());
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            port.set_result(SubagentDiscovery {
                sources: vec![child.clone()],
                bytes_read: child.bytes.len() as u64,
                incomplete: true,
                availability: SubagentDiscoveryAvailability::Unavailable,
                ..Default::default()
            });
            let result = recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await;
            assert_stored_child_body(&storage, &child.bytes);
            let gate = parent_gate(&conn).await;
            assert_eq!(gate["requirements"][0]["minimum_revision"], 1);
            assert_eq!(
                gate["preparing"], true,
                "unavailable partial must not end preparation"
            );
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    result.unwrap_err().as_ref()
                )
                .is_some()
            );
            set_replay_source(port, child.clone());
            recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            crate::internal::ai::capture::catalog::CaptureCatalogStore::new(conn.clone())
                .apply(&request)
                .await
                .unwrap();
            let live = capture_prepared_discovery(
                &conn,
                &storage,
                &request,
                &SubagentDiscovery {
                    sources: vec![child.clone()],
                    incomplete: true,
                    availability: SubagentDiscoveryAvailability::Unavailable,
                    ..Default::default()
                },
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            assert_eq!(live.skipped_unchanged, 1);
            assert_eq!(
                parent_gate(&conn).await["preparing"],
                true,
                "the live consumer must retain the same unavailable barrier"
            );
            assert_stored_child_body(&storage, &child.bytes);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_zero_source_bounded_partial_does_not_invent_required_claims() {
            let (_dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            port.set_result(SubagentDiscovery {
                incomplete: true,
                ..Default::default()
            });
            recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_eq!(
                parent_gate(&conn).await["requirements"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_new_lifecycle_during_discovery_refuses_old_writes() {
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            set_replay_source(port, recovered_source(dir.path()));
            let concurrent = conn.clone();
            port.before_next(Box::pin(async move {
                concurrent
                    .execute_unprepared("UPDATE agent_session SET sync_revision=sync_revision+1")
                    .await?;
                Ok(())
            }));
            let error = recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                error
                    .chain()
                    .any(|e| e.downcast_ref::<CaptureCatalogError>()
                        == Some(&CaptureCatalogError::ScopeRejected))
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_busy_floor_survives_then_expired_owner_takes_new_revision() {
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let child = recovered_source(dir.path());
            let port = Box::leak(Box::new(ReplayDiscovery::default()));
            set_replay_source(port, child.clone());
            recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_subagent_content_claim SET state='reserved',owner='owned-concurrent',
                 attempt_digest=current_digest, attempt_checkpoint_id='owned-attempt',
                 lease_expires_at=?,fence_token=fence_token+1",
                [(Utc::now().timestamp_millis() + 60_000).into()],
            ))
            .await
            .unwrap();
            let error = recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(2)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some(),
                "{error:#}"
            );
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_eq!(
                parent_gate(&conn).await["requirements"][0]["minimum_revision"],
                2
            );
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE state='reserved' AND owner='owned-concurrent'").await, 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_stored_child_body(&storage, &child.bytes);

            let gate_before_unavailable = parent_gate(&conn).await;
            let error = recover_terminal_children(
                &conn,
                &storage,
                &recovery,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some()
            );
            assert_eq!(
                parent_gate(&conn).await,
                gate_before_unavailable,
                "no port cannot re-arm a prepared gate"
            );
            port.set_result(SubagentDiscovery {
                incomplete: true,
                availability: crate::internal::ai::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable,
                ..Default::default()
            });
            let error = recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some()
            );
            assert_eq!(
                parent_gate(&conn).await,
                gate_before_unavailable,
                "missing roots cannot add a new global barrier"
            );
            set_replay_source(port, child.clone());
            conn.execute_unprepared("UPDATE agent_subagent_content_claim SET lease_expires_at=0")
                .await
                .unwrap();
            recover_with_port(
                &conn,
                &storage,
                &recovery,
                port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(scalar(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE state='idle' AND current_revision=2").await, 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(parent_gate(&conn).await["preparing"], false);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_disabled_capability_retains_policy_and_doctor_retry_reason() {
            use crate::internal::ai::capture::recovery::{
                ArtifactRecoveryOutcome, DoctorReplayRequest, replay_for_doctor_with_child_status,
            };
            let (dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let before = parent_gate(&conn).await;
            let error = recover_terminal_children(
                &conn,
                &storage,
                &recovery,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some()
            );
            let expired = CaptureCommitDeadline::from_budget(Duration::from_millis(1)).unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
            let error = recover_terminal_children(&conn, &storage, &recovery, expired)
                .await
                .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some()
            );

            let unavailable_port = Box::leak(Box::new(ReplayDiscovery::default()));
            unavailable_port.set_result(SubagentDiscovery {
                incomplete: true,
                availability: crate::internal::ai::observed_agents::live_capture::SubagentDiscoveryAvailability::Unavailable,
                ..Default::default()
            });
            let error = recover_with_port(
                &conn,
                &storage,
                &recovery,
                unavailable_port,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    error.as_ref()
                )
                .is_some()
            );
            assert_eq!(parent_gate(&conn).await, before);
            let outcome = replay_for_doctor_with_child_status(DoctorReplayRequest {
                conn: &conn,
                recovery: &recovery,
                storage: &storage,
                root: dir.path(),
                repo_path: &storage,
                now_millis: 2 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS,
                deadline: CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
                manual: true,
            })
            .await
            .unwrap();
            assert_eq!(outcome.outcome, ArtifactRecoveryOutcome::RetryLater);
            assert!(outcome.waiting_for_children);
            assert_eq!(parent_gate(&conn).await, before);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_recover_legacy_receipt_never_selects_a_provider() {
            let (_dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let recovery = recovery_token(&conn, &request).await;
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            let mut metadata: serde_json::Value =
                serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap())
                    .unwrap();
            metadata["capture_catalog_receipts_v1"]["entries"][0]
                .as_object_mut()
                .unwrap()
                .remove("child_gate");
            metadata["capture_catalog_receipts_v1"]["version"] = serde_json::json!(1);
            let before = serde_json::to_string(&metadata).unwrap();
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json=?",
                [before.clone().into()],
            ))
            .await
            .unwrap();
            recover_terminal_children_with_binding(
                &conn,
                &storage,
                &recovery,
                CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap(),
                |_| panic!("pre-gate legacy receipt must not select provider"),
            )
            .await
            .unwrap();
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.try_get_by::<String, _>("metadata_json").unwrap(),
                before
            );
        }

        async fn capture_prepared_child(
            conn: &DatabaseConnection,
            storage: &Path,
            request: &CaptureCatalogApplyRequest,
            child: &DiscoveredSubagentContent,
            budget: Duration,
        ) -> Result<SubagentCaptureSummary> {
            capture_prepared_discovery(
                conn,
                storage,
                request,
                &SubagentDiscovery {
                    sources: vec![child.clone()],
                    ..Default::default()
                },
                budget,
            )
            .await
        }

        async fn capture_prepared_discovery(
            conn: &DatabaseConnection,
            storage: &Path,
            request: &CaptureCatalogApplyRequest,
            discovery: &SubagentDiscovery,
            budget: Duration,
        ) -> Result<SubagentCaptureSummary> {
            let program = crate::utils::test::real_libra_helper_binary();
            anyhow::ensure!(
                program.is_file(),
                "build the current CLI before local prepared-child tests"
            );
            let deadline = CaptureCommitDeadline::from_budget(budget)?;
            crate::internal::ai::authorized_read::with_test_helper_program(
                program,
                capture_subagent_contents_for_checkpoint(
                    conn,
                    storage,
                    request,
                    discovery,
                    Some(deadline),
                ),
            )
            .await
        }

        async fn parent_gate(conn: &DatabaseConnection) -> serde_json::Value {
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session WHERE session_id='parent-session'"
                        .to_string(),
                ))
                .await
                .unwrap()
                .unwrap();
            let metadata: serde_json::Value =
                serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap())
                    .unwrap();
            metadata["capture_catalog_receipts_v1"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find_map(|entry| entry.get("child_gate").cloned())
                .unwrap()
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_prepare_root_writes_readable_body_and_unchanged_floor_survives() {
            let (_dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let child = source();
            let missing = capture_subagent_contents_for_checkpoint(
                &conn,
                &storage,
                &request,
                &SubagentDiscovery {
                    sources: vec![child.clone()],
                    ..Default::default()
                },
                None,
            )
            .await
            .expect_err("gated IO needs the original caller budget");
            assert!(
                crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                    missing.as_ref()
                )
                .is_none()
            );
            assert!(
                missing
                    .chain()
                    .any(|cause| cause.downcast_ref::<CaptureCatalogError>()
                        == Some(&CaptureCatalogError::InvalidRequest))
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );

            let first =
                capture_prepared_child(&conn, &storage, &request, &child, Duration::from_secs(5))
                    .await
                    .unwrap();
            assert_eq!(first.checkpoints_written, 1);
            assert_eq!(first.child_requirements.len(), 1);
            assert_eq!(first.child_requirements[0].minimum_revision(), 1);
            assert_stored_child_body(&storage, &child.bytes);
            let gate = parent_gate(&conn).await;
            assert_eq!(gate["preparing"], false);
            assert_eq!(gate["requirements"][0]["minimum_revision"], 1);
            crate::internal::ai::capture::catalog::CaptureCatalogStore::new(conn.clone())
                .apply(&request)
                .await
                .unwrap();
            let replay =
                capture_prepared_child(&conn, &storage, &request, &child, Duration::from_secs(5))
                    .await
                    .unwrap();
            assert_eq!(replay.checkpoints_written, 0);
            assert_eq!(replay.skipped_unchanged, 1);
            assert_eq!(replay.child_requirements[0].minimum_revision(), 1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_stored_child_body(&storage, &child.bytes);
            assert_eq!(parent_gate(&conn).await, gate);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_prepare_inflight_persists_next_floor_and_expired_same_body_advances() {
            let (_dir, conn, storage) = store().await;
            let child = source();
            capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            let request = prepare_parent_request(&conn).await;
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "UPDATE agent_subagent_content_claim SET state='reserved',owner='other-live-owner',lease_expires_at=?,attempt_digest=?,attempt_checkpoint_id='other-attempt'",
                [(Utc::now().timestamp_millis()+60000).into(),"f".repeat(64).into()])).await.unwrap();
            let error = TEST_SUBAGENT_CONTENT_FAILPOINT
                .scope(
                    Some("after_inflight_requirement"),
                    capture_prepared_child(
                        &conn,
                        &storage,
                        &request,
                        &child,
                        Duration::from_secs(5),
                    ),
                )
                .await
                .expect_err("stop only after a durable in-flight requirement");
            assert!(format!("{error:#}").contains("after_inflight_requirement"));
            let progress = error
                .downcast_ref::<SubagentCaptureProgressError>()
                .unwrap();
            assert_eq!(progress.summary().child_requirements.len(), 1, "{error:#}");
            assert_eq!(
                progress.summary().child_requirements[0].minimum_revision(),
                2
            );
            let gate = parent_gate(&conn).await;
            assert_eq!(gate["preparing"], true);
            assert_eq!(gate["requirements"][0]["minimum_revision"], 2);
            assert_eq!(scalar(&conn,"SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE owner='other-live-owner' AND state='reserved'").await,1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            conn.execute_unprepared("UPDATE agent_subagent_content_claim SET lease_expires_at=0")
                .await
                .unwrap();
            crate::internal::ai::capture::catalog::CaptureCatalogStore::new(conn.clone())
                .apply(&request)
                .await
                .unwrap();
            let retry =
                capture_prepared_child(&conn, &storage, &request, &child, Duration::from_secs(5))
                    .await
                    .unwrap();
            assert_eq!(retry.checkpoints_written, 1);
            assert_eq!(retry.skipped_unchanged, 0);
            assert_eq!(parent_gate(&conn).await["preparing"], false);
            assert_eq!(
                parent_gate(&conn).await["requirements"][0]["minimum_revision"],
                2
            );
            assert_eq!(scalar(&conn,"SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE state='idle' AND current_revision=2 AND revision_cursor=2").await,1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_stored_child_body(&storage, &child.bytes);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_prepare_catalog_deadline_is_bounded_through_root() {
            let (_dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let child = source();
            let error = crate::internal::ai::capture::catalog::with_child_preparation_before_dml_delay_for_test(
                Duration::from_secs(6),
                capture_prepared_child(&conn, &storage, &request, &child, Duration::from_secs(5)),
            ).await.expect_err("catalog observes the real original deadline before enrollment");
            assert!(
                error.chain().any(|cause| matches!(
                    cause.downcast_ref::<CaptureCatalogError>(),
                    Some(CaptureCatalogError::DeadlineExceeded)
                )),
                "{error:#}"
            );
            assert!(
                capture_deadline_exhausted(&error),
                "catalog child deadline must preserve the parent partial path: {error:#}"
            );
            assert!(capture_deadline_exhausted(
                &error
                    .context("child preparation")
                    .context("live checkpoint")
            ));
            for other in [
                CaptureCatalogError::ScopeRejected,
                CaptureCatalogError::ChildCheckpointsPending,
                CaptureCatalogError::Database,
            ] {
                assert!(!capture_deadline_exhausted(&anyhow::Error::new(other)));
            }
            assert_eq!(parent_gate(&conn).await["preparing"], true);
            assert_eq!(
                parent_gate(&conn).await["requirements"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_prepare_root_capacity_refuses_a_new_claim_without_discarding_pending()
        {
            let (_dir, conn, storage) = store().await;
            let request = prepare_parent_request(&conn).await;
            let requirements: Vec<_> = (0..16).map(|i| serde_json::json!({
                "source_key": format!("source/subagent-hmac-v2/{i:064x}"), "minimum_revision": 1
            })).collect();
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json=json_set(metadata_json, '$.capture_catalog_receipts_v1.entries[0].child_gate.requirements', json(?)) WHERE session_id='parent-session'",
                [serde_json::to_string(&requirements).unwrap().into()])).await.unwrap();
            let before = parent_gate(&conn).await;
            let error = capture_prepared_child(
                &conn,
                &storage,
                &request,
                &source(),
                Duration::from_secs(5),
            )
            .await
            .expect_err("capacity cannot discard unsettled obligations");
            assert!(
                matches!(
                    crate::internal::ai::capture::live_checkpoint::child_checkpoint_wait_error(
                        error.as_ref()
                    ),
                    Some(CaptureCatalogError::ChildGateCapacity)
                ),
                "{error:#}"
            );
            assert_eq!(parent_gate(&conn).await, before);
            assert_eq!(parent_gate(&conn).await["preparing"], true);
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_prepare_stale_action_blocks_claim_release_and_final_transaction() {
            let (_dir, conn, storage) = store().await;
            let mut child = source();
            capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .unwrap();
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT source_key FROM agent_subagent_content_claim".to_string(),
                ))
                .await
                .unwrap()
                .unwrap();
            child.source_key = row.try_get_by("source_key").unwrap();
            let request = prepare_parent_request(&conn).await;
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(5)).unwrap();
            let txn = begin_subagent_write_transaction_until(
                &conn,
                deadline,
                "owned preparation snapshot",
            )
            .await
            .unwrap();
            let fence = child_preparation_fence(&txn, &request)
                .await
                .unwrap()
                .unwrap();
            txn.rollback().await.unwrap();
            let digest = "f".repeat(64);
            let reserved = reserve_source_prepared(
                &conn,
                request.scope(),
                "parent-session",
                &child,
                SourceReservationAttempt {
                    attempt: ReservationAttempt {
                        content_digest: &digest,
                        checkpoint_id: "new-child-attempt",
                        owner: "owned-old-action",
                    },
                    preparation: Some(&fence),
                },
                &UnchangedDurabilityProof::default(),
                deadline,
            )
            .await
            .unwrap();
            let (fence_token, revision) = match reserved.outcome {
                ReservationOutcome::Reserved {
                    fence_token,
                    revision,
                } => (fence_token, revision),
                _ => panic!("owned reservation"),
            };
            conn.execute_unprepared("UPDATE agent_session SET sync_revision=sync_revision+1")
                .await
                .unwrap();
            let plan = ContentCommitPlan {
                preparation: Some(fence.clone()),
                capture_scope: request.scope().clone(),
                parent_session_id: "parent-session".into(),
                provider_kind: "codex".into(),
                source_key: child.source_key.clone(),
                content_digest: digest,
                checkpoint_id: "new-child-attempt".into(),
                owner: "owned-old-action".into(),
                fence_token,
                revision,
                parent_commit: None,
                source_channel: "live".into(),
                partial: false,
                stable_subagent_id: child.stable_subagent_id.clone(),
                codex_binding: child.codex_binding.clone(),
                created_at: 2,
                deadline,
            };
            let txn = begin_subagent_write_transaction_until(
                &conn,
                deadline,
                "owned stale final transaction",
            )
            .await
            .unwrap();
            let error = plan
                .apply(
                    &txn,
                    &TracesCommitCtx {
                        commit_hash: "unused".into(),
                        tree_oid: "unused".into(),
                        metadata_blob_oid: "unused".into(),
                    },
                )
                .await
                .expect_err("old action cannot finalize");
            assert!(
                error
                    .downcast_ref::<CaptureCatalogError>()
                    .is_some_and(|e| matches!(e, CaptureCatalogError::ScopeRejected))
            );
            txn.rollback().await.unwrap();
            let error = release_reservation(
                &conn,
                request.scope(),
                ReservationRelease {
                    preparation: Some(&fence),
                    parent_session_id: "parent-session",
                    source: &child,
                    checkpoint_id: "new-child-attempt",
                    owner: "owned-old-action",
                    fence_token,
                },
                deadline,
            )
            .await
            .expect_err("old action cannot mutate the claim");
            assert!(
                error
                    .downcast_ref::<CaptureCatalogError>()
                    .is_some_and(|e| matches!(e, CaptureCatalogError::ScopeRejected))
            );
            assert_eq!(scalar(&conn,"SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE state='reserved' AND owner='owned-old-action'").await,1);
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_writer_preserves_projection_binding_without_debug_content() {
            let source = source();
            let projection =
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(5))
                    .await
                    .expect("bound projection");
            assert_eq!(projection.transcript.as_ref(), source.bytes.as_slice());
            assert!(projection.codex_binding == source.codex_binding);
            let debug = format!("{projection:?}");
            for value in [
                "WRITER_CHILD_THINKING_BODY",
                "opaque-preserved",
                CHILD,
                TURN,
                "123e4567-e89b-12d3-a456-426614174031",
            ] {
                assert!(
                    !debug.contains(value),
                    "Debug must omit source content and identity"
                );
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_writer_stores_body_and_never_guesses_boundary_from_child_id() {
            let (_directory, conn, storage) = store().await;
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_checkpoint (
                    checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
                    traces_commit, tool_use_id, subagent_session_id, description, created_at
                 ) VALUES ('cid-only-boundary', 'parent-session', 'subagent',
                           'not-a-proof-tree', 'not-a-proof-metadata', 'not-a-proof-commit',
                           NULL, ?, 'unproved child id only', 1)",
                [CHILD.into()],
            ))
            .await
            .expect("CID-only boundary candidate");
            let source = source();
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("durable Codex child content");
            assert_eq!(first.checkpoints_written, 1);
            assert_stored_child_body(&storage, &source.bytes);
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT cp.checkpoint_id, cp.metadata_blob_oid, cp.subagent_session_id,
                        l.link_state, l.boundary_checkpoint_id, l.stable_subagent_id
                 FROM agent_subagent_content_claim c
                 JOIN agent_checkpoint cp ON cp.checkpoint_id = c.current_checkpoint_id
                 JOIN agent_subagent_link l ON l.content_checkpoint_id = cp.checkpoint_id"
                        .to_string(),
                ))
                .await
                .expect("read durable child")
                .expect("child exists");
            assert!(
                row.try_get_by::<Option<String>, _>("subagent_session_id")
                    .expect("SID")
                    .is_none()
            );
            assert_eq!(
                row.try_get_by::<String, _>("link_state").expect("state"),
                "unresolved"
            );
            assert!(
                row.try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                    .expect("boundary")
                    .is_none()
            );
            assert_eq!(
                row.try_get_by::<String, _>("stable_subagent_id")
                    .expect("child ID"),
                CHILD
            );
            let metadata_oid: String = row.try_get_by("metadata_blob_oid").expect("metadata OID");
            let metadata: serde_json::Value =
                serde_json::from_slice(&blob(&storage, &metadata_oid)).expect("stored metadata");
            assert!(metadata["subagent_session_id"].is_null());
            assert_eq!(metadata["subagent"]["link_state"], "unresolved");
            assert_eq!(
                metadata["subagent"]["source_identity"]["root"],
                "123e4567-e89b-12d3-a456-426614174031"
            );
            assert_eq!(
                metadata["subagent"]["source_identity"]["parent"],
                "parent-thread"
            );
            assert_eq!(metadata["subagent"]["source_identity"]["child"], CHILD);
            assert_eq!(metadata["subagent"]["raw_byte_count"], source.bytes.len());
            let replay = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source],
                "import",
                None,
            )
            .await
            .expect("unchanged replay without CID inference");
            assert_eq!(replay.skipped_unchanged, 1);
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
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_writer_rejects_different_capture_before_claim() {
            let (_directory, conn, storage) = store().await;
            let mut source = source();
            source
                .codex_binding
                .as_mut()
                .expect("fixture binding")
                .capture_session = "another-session".into();
            let error = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect_err("cross-capture source must not be persisted");
            assert!(format!("{error:#}").contains("does not match this capture session"));
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                0
            );
        }
        fn bind_associations(
            summary: &SubagentCaptureSummary,
            cwd: &Path,
            event_id: uuid::Uuid,
            kind: crate::internal::ai::hooks::LifecycleEventKind,
            turn: &str,
        ) -> SubagentBoundaryAssociations {
            use crate::internal::ai::{
                hooks::{LifecycleEventKind, SessionHookEnvelope, providers::codex_provider},
                observed_agents::live_capture::{LiveCaptureBinding, LiveCaptureContext},
            };
            let envelope: SessionHookEnvelope = serde_json::from_value(serde_json::json!({
                "hook_event_name": "SubagentStop", "session_id": "123e4567-e89b-12d3-a456-426614174031", "cwd": cwd,
                "agent_id": CHILD, "agent_transcript_path": "/owned/child.jsonl", "turn_id": turn,
            }))
            .expect("synthetic current callback");
            let hints = LiveCaptureBinding::resolve(codex_provider())
                .child_source_hint_policy()
                .extract(&envelope, LifecycleEventKind::SubagentEnd);
            summary.bind_boundary_associations(
                kind,
                event_id,
                &LiveCaptureContext {
                    libra_session_id: "parent-session",
                    provider_session_id: "123e4567-e89b-12d3-a456-426614174031",
                    verified_cwd: cwd,
                },
                &hints,
            )
        }

        fn boundary_id(event: uuid::Uuid) -> String {
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                event,
                crate::internal::ai::capture::state::CheckpointWrite::SubagentBoundary,
            )
        }

        // Component transaction: source objects are real; boundary ctx is
        // synthetic. Full traces/CAS and native-host acceptance are separate.
        async fn apply_associations(
            conn: &DatabaseConnection,
            scope: &CaptureScope,
            associations: &SubagentBoundaryAssociations,
            checkpoint: &str,
        ) -> Result<()> {
            let txn = crate::internal::db::begin_write_transaction(conn).await?;
            let ctx = TracesCommitCtx {
                commit_hash: "c".repeat(40),
                tree_oid: "a".repeat(40),
                metadata_blob_oid: "b".repeat(40),
            };
            let base = crate::internal::ai::capture::checkpoint::SubagentCheckpointCommitPlan::new(
                checkpoint.into(),
                "parent-session".into(),
                None,
                None,
                None,
                None,
                Some("synthetic boundary".into()),
                1,
                scope.clone(),
            )
            .expect("existing boundary plan");
            let result: Result<()> = async {
                base.apply(&txn, &ctx).await?;
                associations
                    .apply(
                        &txn,
                        &ctx,
                        SubagentBoundaryAssociationContext {
                            scope,
                            capture_session: "parent-session",
                            boundary_checkpoint_id: checkpoint,
                            deadline: Some(test_mutation_deadline()),
                        },
                    )
                    .await
            }
            .await;
            match result {
                Ok(()) => txn.commit().await.context("commit test companion"),
                Err(error) => {
                    txn.rollback().await.expect("rollback rejected companion");
                    Err(error)
                }
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_unchanged_import_gains_only_fresh_live_proof() {
            use crate::internal::ai::hooks::LifecycleEventKind;
            let (directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn)
                .await
                .expect("scope");
            let source = source();
            let mut imported_source = source.clone();
            imported_source.partial = true;
            imported_source.codex_binding.as_mut().unwrap().hook_turn = None;
            let imported = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&imported_source),
                "import",
                None,
            )
            .await
            .expect("ordinary import first");
            assert!(imported.association_proofs.is_empty());
            assert_eq!(imported.partial_sources, 1);
            let live = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect("fresh same snapshot Stop");
            assert_eq!(live.skipped_unchanged, 1);
            assert_eq!(live.association_proofs.len(), 1);
            assert_eq!(live.association_proofs[0].observed_turn, TURN);
            let event = uuid::Uuid::new_v4();
            let bound = bind_associations(
                &live,
                directory.path(),
                event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            assert_eq!(bound.proofs.len(), 1);
            let wrong = boundary_id(uuid::Uuid::new_v4());
            apply_associations(&conn, &scope, &bound, &wrong)
                .await
                .expect("wrong target stays unresolved");
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                0
            );
            let expected = boundary_id(event);
            apply_associations(&conn, &scope, &bound, &expected)
                .await
                .expect("exact target");
            apply_associations(&conn, &scope, &bound, &expected)
                .await
                .expect("idempotent association");
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT boundary_checkpoint_id, sync_revision FROM agent_subagent_link"
                        .to_string(),
                ))
                .await
                .expect("link")
                .expect("one link");
            assert_eq!(
                row.try_get_by::<String, _>("boundary_checkpoint_id")
                    .expect("boundary"),
                expected
            );
            assert_eq!(
                row.try_get_by::<i64, _>("sync_revision").expect("revision"),
                2
            );
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
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_stale_revision_is_not_used_or_overwritten() {
            use crate::internal::ai::hooks::LifecycleEventKind;
            let (directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn)
                .await
                .expect("scope");
            let mut source = source();
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("first snapshot");
            assert_eq!(first.association_proofs.len(), 1);
            let event = uuid::Uuid::new_v4();
            let stale = bind_associations(
                &first,
                directory.path(),
                event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            source.bytes.extend_from_slice(
                b"{\"type\":\"security_risk_score\",\"payload\":{\"scores\":{}}}\n",
            );
            let second = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect("later exact content");
            assert_eq!(second.association_proofs.len(), 1);
            assert_eq!(second.association_proofs[0].revision, 2);
            let target = boundary_id(event);
            apply_associations(&conn, &scope, &stale, &target)
                .await
                .expect("raced claim stays unresolved");
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                0
            );
            let fresh = bind_associations(
                &second,
                directory.path(),
                event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            apply_associations(&conn, &scope, &fresh, &target)
                .await
                .expect("fresh revision may resolve");
            let other_event = uuid::Uuid::new_v4();
            let other = bind_associations(
                &second,
                directory.path(),
                other_event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            apply_associations(&conn, &scope, &other, &boundary_id(other_event))
                .await
                .expect("must not overwrite");
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                1
            );
            let link = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
                "SELECT boundary_checkpoint_id FROM agent_subagent_link WHERE link_state = 'resolved'".to_string(),
            )).await.expect("link").expect("resolved row");
            assert_eq!(
                link.try_get_by::<String, _>("boundary_checkpoint_id")
                    .expect("target"),
                target
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_wrong_callback_and_partial_keep_body_without_proof() {
            use crate::internal::ai::hooks::LifecycleEventKind;
            let (directory, conn, storage) = store().await;
            let mut source = source();
            let live = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("complete source");
            let failed_progress = live.clone().without_association_proofs();
            assert!(failed_progress.association_proofs.is_empty());
            assert_eq!(
                failed_progress.checkpoints_written,
                live.checkpoints_written
            );
            assert_eq!(failed_progress.discovered, live.discovered);
            let event = uuid::Uuid::new_v4();
            for (kind, turn) in [
                (LifecycleEventKind::SubagentStart, TURN),
                (
                    LifecycleEventKind::SubagentEnd,
                    "01991d45-6f00-7000-8000-000000000002",
                ),
            ] {
                assert!(
                    bind_associations(&live, directory.path(), event, kind, turn)
                        .proofs
                        .is_empty()
                );
            }
            source.partial = true;
            let retained_bytes = source.bytes.clone();
            let partial = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect("ordinary partial must stay durable");
            assert_eq!(partial.checkpoints_written, 0);
            assert_eq!(partial.skipped_unchanged, 1);
            assert_stored_child_body(&storage, &retained_bytes);
            assert!(partial.association_proofs.is_empty());
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
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_new_capture_epoch_rolls_back_boundary_only() {
            use crate::internal::ai::hooks::LifecycleEventKind;
            let (directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn)
                .await
                .expect("scope");
            let summary = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source()],
                "live",
                None,
            )
            .await
            .expect("durable source");
            let event = uuid::Uuid::new_v4();
            let bound = bind_associations(
                &summary,
                directory.path(),
                event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            assert_eq!(bound.proofs.len(), 1);
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json = json_object('capture_incarnation', ?) WHERE session_id = 'parent-session'",
                ["a".repeat(32).into()],
            )).await.expect("new capture incarnation");
            let id = boundary_id(event);
            let error = apply_associations(&conn, &scope, &bound, &id)
                .await
                .expect_err("stale epoch");
            assert!(error.to_string().contains("earlier capture"));
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved'"
                )
                .await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_write_failure_rolls_back_boundary_preserving_content() {
            use crate::internal::ai::hooks::LifecycleEventKind;
            let (directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn)
                .await
                .expect("scope");
            let summary = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[source()],
                "live",
                None,
            )
            .await
            .expect("durable child content");
            assert_eq!(summary.association_proofs.len(), 1);
            let event = uuid::Uuid::new_v4();
            let bound = bind_associations(
                &summary,
                directory.path(),
                event,
                LifecycleEventKind::SubagentEnd,
                TURN,
            );
            conn.execute_raw(Statement::from_string(conn.get_database_backend(),
                "CREATE TRIGGER local_child_link_failure BEFORE UPDATE OF link_state ON agent_subagent_link BEGIN SELECT RAISE(ABORT, 'synthetic association write failure'); END".to_string(),
            )).await.expect("inject owned SQL failure");
            let id = boundary_id(event);
            let error = apply_associations(&conn, &scope, &bound, &id)
                .await
                .expect_err("association DML failure must propagate");
            assert!(error.to_string().contains("associate exact child revision"));
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1,
                "only the previously committed content remains"
            );
            assert_eq!(scalar(&conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL"
            ).await, 1);
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "DROP TRIGGER local_child_link_failure".to_string(),
            ))
            .await
            .expect("remove owned fault");
            apply_associations(&conn, &scope, &bound, &id)
                .await
                .expect("same-event retry");
            assert_eq!(
                scalar(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(scalar(&conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'resolved' AND sync_revision = 2"
            ).await, 1);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_new_start_preserves_body_without_stale_proof() {
            let (_directory, conn, storage) = store().await;
            let mut child = source();
            let later = "01991d45-6f00-7000-8000-000000000002";
            child.bytes.extend_from_slice(format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"{later}\"}}}}\n"
            ).as_bytes());
            let first = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&child),
                "live",
                None,
            )
            .await
            .expect("ordinary child capture");
            assert_eq!(first.checkpoints_written, 1);
            assert!(
                first.association_proofs.is_empty(),
                "old callback must not mint a stale proof"
            );
            let mut body_stored = false;
            for prefix in std::fs::read_dir(storage.join("objects")).expect("objects") {
                let prefix = prefix.expect("prefix");
                if prefix.file_name().to_string_lossy().len() != 2 || !prefix.path().is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(prefix.path()).expect("object prefix") {
                    let bytes = std::fs::read(entry.expect("object").path()).expect("object bytes");
                    let mut decoded = Vec::new();
                    flate2::read::ZlibDecoder::new(bytes.as_slice())
                        .read_to_end(&mut decoded)
                        .expect("decode loose object");
                    if let Some(end) = decoded.iter().position(|byte| *byte == 0)
                        && decoded[..end].starts_with(b"blob ")
                        && decoded[end + 1..] == child.bytes
                    {
                        body_stored = true;
                    }
                }
            }
            assert!(
                body_stored,
                "a new start must not discard previously obtained thinking"
            );
            assert_eq!(scalar(&conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL"
            ).await, 1);

            child.bytes.extend_from_slice(format!(
                "{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{later}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"text\":\"LATER_TURN_THINKING\"}}}}\n"
            ).as_bytes());
            child.codex_binding.as_mut().expect("binding").hook_turn = Some(later.into());
            let fresh = capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                &[child],
                "live",
                None,
            )
            .await
            .expect("fresh current context");
            assert_eq!(fresh.checkpoints_written, 1);
            assert_eq!(fresh.association_proofs.len(), 1);
            assert_eq!(fresh.association_proofs[0].observed_turn, later);
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_association_busy_or_expired_reserved_claim_never_mints_proof() {
            let (_directory, conn, storage) = store().await;
            let scope = CaptureScope::main_for_connection(&conn)
                .await
                .expect("scope");
            let mut source = source();
            capture_discovered_subagent_contents(
                &conn,
                &storage,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("durable source");
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT source_key, current_checkpoint_id FROM agent_subagent_content_claim"
                        .to_string(),
                ))
                .await
                .expect("claim")
                .expect("one claim");
            source.source_key = row.try_get_by("source_key").expect("source key");
            let checkpoint: String = row.try_get_by("current_checkpoint_id").expect("checkpoint");
            let projected =
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(5))
                    .await
                    .expect("fresh snapshot");
            let prepared = PreparedSubagentContent {
                source: &source,
                transcript: projected.transcript,
                report_value: projected.report_value,
                partial: projected.partial,
                turn_count: projected.turn_count,
                content_digest: hex::encode(projected.content_digest),
                codex_binding: projected.codex_binding,
                codex_usage: projected.codex_usage,
            };
            for lease in [Utc::now().timestamp_millis() + 60_000, 1] {
                conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                    "UPDATE agent_subagent_content_claim SET state = 'reserved', owner = 'another-owner',
                     lease_expires_at = ?, attempt_digest = current_digest, attempt_checkpoint_id = 'next-attempt'",
                    [lease.into()],
                )).await.expect("reserved owner");
                let mut summary = SubagentCaptureSummary::default();
                record_association_proof(
                    &conn,
                    AssociationProofInput {
                        scope: &scope,
                        capture_session: "parent-session",
                        capture_incarnation: None,
                        source_channel: "live",
                        item: &prepared,
                        checkpoint_id: &checkpoint,
                    },
                    &mut summary,
                    test_mutation_deadline(),
                )
                .await
                .expect("no takeover");
                assert!(
                    summary.association_proofs.is_empty(),
                    "a lease expiry is not a settlement proof"
                );
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
#[cfg(any(unix, windows))]
mod codex_child_discovery_local_tests {
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_child_hint_legacy_live_is_neutral_without_gating_stored_discovery() {
        use crate::internal::ai::{
            hooks::providers::codex_provider,
            observed_agents::live_capture::{
                LiveCaptureBinding, LiveCaptureContext, StoredSubagentSelection,
                SubagentDiscoveryAvailability, SubagentDiscoveryLimits, UntrustedChildSourceHints,
            },
        };

        let directory = tempfile::tempdir().unwrap();
        let binding = LiveCaptureBinding::resolve(codex_provider());
        let port = binding.subagent_discovery().unwrap();
        let sentinel = SubagentDiscovery {
            warning: Some("operator-local Root dispatch sentinel".into()),
            incomplete: true,
            availability: SubagentDiscoveryAvailability::Unavailable,
            ..SubagentDiscovery::default()
        };
        with_subagent_discovery_override(sentinel.clone(), async {
            for label in ["oracle-codex-source-absent", "legacy-root-label"] {
                let capture_session = format!("codex__{label}");
                let context = LiveCaptureContext {
                    libra_session_id: &capture_session,
                    provider_session_id: label,
                    verified_cwd: directory.path(),
                };
                for deadline in [
                    Instant::now() + Duration::from_secs(5),
                    Instant::now() - Duration::from_secs(1),
                ] {
                    let result = port
                        .discover_with_hint(
                            &context,
                            &UntrustedChildSourceHints::Unavailable,
                            deadline,
                        )
                        .await
                        .unwrap();
                    assert!(result.sources.is_empty());
                    assert_eq!(result.bytes_read, 0);
                    assert!(
                        result.warning.is_none(),
                        "legacy live must not dispatch Root"
                    );
                    assert!(!result.incomplete);
                    assert_eq!(
                        result.availability,
                        SubagentDiscoveryAvailability::Available
                    );
                }
                let result = port
                    .discover(&context, Instant::now() + Duration::from_secs(5))
                    .await
                    .unwrap();
                assert!(
                    result.warning.is_none(),
                    "plain discover must share live qualification"
                );

                for selection in [
                    StoredSubagentSelection::ImportCurrent {
                        parent_locator: None,
                    },
                    StoredSubagentSelection::TerminalRecoveryRoot,
                ] {
                    let stored = port
                        .discover_stored(
                            &context,
                            selection,
                            SubagentDiscoveryLimits {
                                byte_budget: 16 * 1024 * 1024,
                                source_limit: 16,
                            },
                            Instant::now() + Duration::from_secs(5),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        stored.warning, sentinel.warning,
                        "the stored port must keep its existing scoped Root selection"
                    );
                    assert!(stored.incomplete);
                    assert_eq!(
                        stored.availability,
                        SubagentDiscoveryAvailability::Unavailable
                    );
                    assert!(stored.sources.is_empty());
                    assert_eq!(stored.bytes_read, 0);
                }
            }
            let native = LiveCaptureContext {
                libra_session_id: "codex__123e4567-e89b-12d3-a456-426614174031",
                provider_session_id: "123e4567-e89b-12d3-a456-426614174031",
                verified_cwd: directory.path(),
            };
            let result = port
                .discover_with_hint(
                    &native,
                    &UntrustedChildSourceHints::Absent,
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(
                result.warning, sentinel.warning,
                "a supported UUID must still reach Root with all real outcomes"
            );
            assert!(result.incomplete);
            assert_eq!(
                result.availability,
                SubagentDiscoveryAvailability::Unavailable
            );
            assert!(result.sources.is_empty());
            assert_eq!(result.bytes_read, 0);
        })
        .await;
    }

    use super::*;

    const TURN: &str = "01991d45-6f00-7000-8000-000000000001";
    const CHILD: &str = "child-thread";
    const PARENT: &str = "parent-thread";
    const ROOT: &str = "root-thread";

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        cwd: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("functional Codex fixture");
            let normalize = super::super::observed_agents::transcript_source::normalize_macos_system_directory_alias;
            let root = normalize(&temp.path().join("codex"));
            let cwd = normalize(&temp.path().join("worktree"));
            std::fs::create_dir_all(root.join("sessions/2026/10/20")).expect("rollout fixture");
            std::fs::create_dir_all(&cwd).expect("owned workspace fixture");
            Self {
                _temp: temp,
                root,
                cwd,
            }
        }

        fn path(&self, id: &str) -> PathBuf {
            self.root
                .join("sessions/2026/10/20")
                .join(format!("rollout-local-{id}.jsonl"))
        }

        fn header(&self, id: &str, root: &str, parent: Option<&str>, cwd: &Path) -> Vec<u8> {
            let mut bytes = serde_json::to_vec(&serde_json::json!({
                "type": "session_meta",
                "payload": {"id": id, "session_id": root, "parent_thread_id": parent, "cwd": cwd},
            }))
            .expect("owned header");
            bytes.push(b'\n');
            bytes
        }

        fn child(&self, text: &str) -> Vec<u8> {
            self.child_of(PARENT, text)
        }

        fn child_of(&self, parent: &str, text: &str) -> Vec<u8> {
            let mut bytes = self.header(CHILD, ROOT, Some(parent), &self.cwd);
            bytes.extend_from_slice(format!(
                "{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{TURN}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"{text}\"}}],\"encrypted_content\":\"opaque-child\"}}}}\n{{\"type\":\"token_usage_record\",\"payload\":{{\"thread_id\":\"{CHILD}\",\"session_id\":\"{ROOT}\",\"turn_id\":\"{TURN}\",\"root_turn_id\":\"root-turn-differs\"}}}}\n"
            ).as_bytes());
            bytes
        }

        fn parent(&self) {
            self.parent_at(PARENT);
        }

        fn parent_at(&self, parent: &str) {
            let mut bytes = self.header(parent, ROOT, None, &self.cwd);
            bytes.extend_from_slice(format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"collab_agent_spawn_end\",\"sender_thread_id\":\"{parent}\",\"new_thread_id\":\"{CHILD}\"}}}}\n"
            ).as_bytes());
            std::fs::write(self.path(parent), bytes).expect("parent fixture");
        }

        fn query(&self) -> CodexChildDiscoveryQuery {
            CodexChildDiscoveryQuery {
                capture_session: "codex__root-thread".into(),
                selection: CodexChildDiscoverySelection::Boundary {
                    root: ROOT.into(),
                    child_id: CHILD.into(),
                    child_path: self.path(CHILD).to_str().expect("UTF-8 fixture").into(),
                    parent_path: Some(self.path(PARENT).to_str().expect("UTF-8 fixture").into()),
                    turn_id: Some(TURN.into()),
                },
            }
        }

        fn pin(&self) -> Vec<CodexPinnedDiscoveryRoot> {
            vec![CodexPinnedDiscoveryRoot {
                path: self.root.clone(),
                directory: super::super::observed_agents::transcript_source::open_absolute_directory_no_follow(&self.root)
                    .expect("pin owned functional root"),
            }]
        }

        fn run(
            &self,
            query: &CodexChildDiscoveryQuery,
            cap: u64,
            limit: usize,
        ) -> SubagentDiscovery {
            discover_codex_subagent_contents_in_roots(
                &self.cwd,
                query,
                CodexDiscoveryBudget {
                    spent: 0,
                    cap,
                    deadline: Instant::now() + Duration::from_secs(5),
                },
                limit,
                self.pin(),
                false,
            )
            .expect("bounded discovery")
        }

        fn candidate(&self) -> CodexDiscoveryCandidate {
            CodexDiscoveryCandidate {
                root: 0,
                relative: self
                    .path(CHILD)
                    .strip_prefix(&self.root)
                    .expect("owned relative")
                    .into(),
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_legacy_root_uses_sdk_fallback_without_child_inference() {
        let f = Fixture::new();
        let parent = serde_json::json!({"type":"session_meta","payload":{"id":PARENT,"cwd":f.cwd}});
        let edge = serde_json::json!({"type":"event_msg","payload":{"type":"collab_agent_spawn_end","sender_thread_id":PARENT,"new_thread_id":CHILD}});
        std::fs::write(f.path(PARENT), format!("{parent}\n{edge}\n")).unwrap();
        let mut child = f.header(CHILD, PARENT, Some(PARENT), &f.cwd);
        let body = serde_json::json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"LEGACY_ROOT_CHILD_BODY"}]}});
        child.extend_from_slice(format!("{body}\n").as_bytes());
        std::fs::write(f.path(CHILD), child).unwrap();
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__parent-thread".into(),
            selection: CodexChildDiscoverySelection::Import {
                current: PARENT.into(),
                current_path: Some(f.path(PARENT).to_str().unwrap().to_owned()),
            },
        };
        let result = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(result.sources.len(), 1);
        assert!(
            !result.incomplete && !result.sources[0].partial,
            "the pinned SDK infers a legacy root's missing session_id from id"
        );
        assert!(
            String::from_utf8_lossy(&result.sources[0].bytes).contains("LEGACY_ROOT_CHILD_BODY")
        );
        for root in [
            serde_json::Value::Null,
            serde_json::json!(42),
            serde_json::json!(""),
        ] {
            let mut invalid = parent.clone();
            invalid["payload"]["session_id"] = root;
            assert!(
                parse_codex_discovery_header(
                    format!("{invalid}\n").as_bytes(),
                    &f.cwd,
                    CodexHeaderMode::Strict
                )
                .is_err()
            );
        }
        let missing_child_root = serde_json::json!({"type":"session_meta","payload":{"id":CHILD,"parent_thread_id":PARENT,"cwd":f.cwd}});
        assert!(
            parse_codex_discovery_header(
                format!("{missing_child_root}\n").as_bytes(),
                &f.cwd,
                CodexHeaderMode::Strict
            )
            .is_err(),
            "a child must not infer its root from its own id or a caller hint"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_recover_availability_wire_separates_truncation_from_unavailable() {
        use super::super::observed_agents::live_capture::SubagentDiscoveryAvailability;
        let f = Fixture::new();
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__root-thread".into(),
            selection: CodexChildDiscoverySelection::Recovery { root: ROOT.into() },
        };
        let decode = |frame| {
            decode_subagent_discovery_helper_frame_with_query(
                frame,
                TRANSCRIPT_READ_HARD_CAP_BYTES,
                16,
                Instant::now() + Duration::from_secs(1),
                Some((&query, &f.cwd)),
            )
        };
        for availability in [
            SubagentDiscoveryAvailability::Available,
            SubagentDiscoveryAvailability::Unavailable,
        ] {
            let frame = encode_subagent_discovery_helper_success_with_query(
                SubagentDiscovery {
                    incomplete: true,
                    availability,
                    ..Default::default()
                },
                Some(&query),
            )
            .unwrap();
            let decoded = decode(frame).unwrap();
            assert_eq!(decoded.availability, availability);
            assert!(decoded.incomplete);
            assert_eq!(decoded.bytes_read, 0);
        }
        let legacy =
            encode_subagent_discovery_helper_success(SubagentDiscovery::default()).unwrap();
        assert_eq!(
            &legacy[4..],
            br#"{"status":"ok","sources":[],"bytes_read":0,"warning":null,"incomplete":false}"#,
            "Claude wire must be byte-identical"
        );
        assert!(
            encode_subagent_discovery_helper_success_with_query(
                SubagentDiscovery {
                    availability: SubagentDiscoveryAvailability::Unavailable,
                    ..Default::default()
                },
                Some(&query),
            )
            .is_err()
        );
        let bad = SubagentDiscoveryHelperResponse::Ok {
            sources: Vec::new(),
            bytes_read: 0,
            metadata_bytes_read: 0,
            codex_protocol: Some(1),
            availability: Some(SubagentDiscoveryAvailability::Unavailable),
            warning: None,
            usage_enumeration: None,
            incomplete: false,
        };
        assert!(decode(encode_subagent_discovery_helper_header(&bad).unwrap()).is_err());
        let older = SubagentDiscoveryHelperResponse::Ok {
            sources: Vec::new(),
            bytes_read: 0,
            metadata_bytes_read: 0,
            codex_protocol: Some(1),
            availability: None,
            warning: None,
            usage_enumeration: None,
            incomplete: true,
        };
        assert_eq!(
            decode(encode_subagent_discovery_helper_header(&older).unwrap())
                .unwrap()
                .availability,
            SubagentDiscoveryAvailability::Unavailable,
            "old unknown empty partial must not assert a usable source environment"
        );
    }

    #[cfg(unix)]
    fn stored_request_helper(
        directory: &Path,
        expected: serde_json::Value,
        response: Vec<u8>,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let helper = directory.join("stored-request-fixture");
        let expected = STANDARD.encode(serde_json::to_vec(&expected).unwrap());
        let response = STANDARD.encode(response);
        std::fs::write(&helper, format!(
            "#!/usr/bin/python3\nimport base64,json,sys,time\nq=json.load(sys.stdin)\nassert 0 < q.pop('remaining_ms') <= 5000\nbound=q.pop('codex_deadline_unix_ms',None)\nif bound is not None:\n assert bound <= int(time.time()*1000)+5000\nassert q == json.loads(base64.b64decode('{expected}'))\nsys.stdout.buffer.write(base64.b64decode('{response}'))\n"
        )).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        helper
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_stored_selection_and_limits_reach_killable_executor() {
        use super::super::observed_agents::live_capture::{
            LiveCaptureContext, StoredSubagentSelection, SubagentDiscoveryLimits,
        };
        for recovery in [false, true] {
            let f = Fixture::new();
            let context = LiveCaptureContext {
                libra_session_id: "codex__stored",
                provider_session_id: if recovery { ROOT } else { PARENT },
                verified_cwd: &f.cwd,
            };
            let selection = if recovery {
                CodexChildDiscoverySelection::Recovery { root: ROOT.into() }
            } else {
                CodexChildDiscoverySelection::Import {
                    current: PARENT.into(),
                    current_path: Some(f.path(PARENT).to_str().unwrap().to_owned()),
                }
            };
            let query = CodexChildDiscoveryQuery {
                capture_session: "codex__stored".into(),
                selection,
            };
            let response = encode_subagent_discovery_helper_success_with_query(
                SubagentDiscovery {
                    incomplete: true,
                    ..SubagentDiscovery::default()
                },
                Some(&query),
            )
            .unwrap();
            let expected = serde_json::json!({
                "cwd_base64": encode_helper_path(&f.cwd),
                "provider_session_id": context.provider_session_id,
                "byte_budget": 731, "source_limit": 1, "codex_query": query,
            });
            let program = stored_request_helper(f._temp.path(), expected, response);
            let parent = f.path(PARENT);
            let selection = if recovery {
                StoredSubagentSelection::TerminalRecoveryRoot
            } else {
                StoredSubagentSelection::ImportCurrent {
                    parent_locator: Some(&parent),
                }
            };
            let discovery = with_subagent_discovery_helper_program(
                program,
                discover_codex_subagent_contents_stored(
                    &context,
                    selection,
                    SubagentDiscoveryLimits {
                        byte_budget: 731,
                        source_limit: 1,
                    },
                    Instant::now() + Duration::from_secs(5),
                ),
            )
            .await
            .expect("helper must accept exact stored selection/caps");
            assert_eq!(discovery.bytes_read, 0);
            assert!(discovery.sources.is_empty());
            assert!(discovery.incomplete);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_stored_absent_live_hint_keeps_parent_selection() {
        use super::super::observed_agents::live_capture::{
            LiveCaptureContext, UntrustedChildSourceHints,
        };
        let f = Fixture::new();
        let context = LiveCaptureContext {
            libra_session_id: "codex__stored",
            provider_session_id: ROOT,
            verified_cwd: &f.cwd,
        };
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__stored".into(),
            selection: CodexChildDiscoverySelection::Parent {
                root: ROOT.into(),
                current_path: None,
            },
        };
        let expected = serde_json::json!({
            "cwd_base64": encode_helper_path(&f.cwd), "provider_session_id": ROOT,
            "byte_budget": TRANSCRIPT_READ_HARD_CAP_BYTES,
            "source_limit": MAX_SUBAGENT_SOURCES_PER_CAPTURE, "codex_query": query,
        });
        let response = encode_subagent_discovery_helper_success_with_query(
            SubagentDiscovery::default(),
            Some(&query),
        )
        .unwrap();
        let program = stored_request_helper(f._temp.path(), expected, response);
        let discovery = with_subagent_discovery_helper_program(
            program,
            discover_codex_subagent_contents_live(
                &context,
                &UntrustedChildSourceHints::Absent,
                Instant::now() + Duration::from_secs(5),
            ),
        )
        .await
        .expect("hint-less live discovery must not widen to root recovery");
        assert!(discovery.sources.is_empty());
        assert!(!discovery.incomplete);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_stored_claude_override_preserves_original_wire_and_caps() {
        use super::super::observed_agents::live_capture::{
            LiveCaptureBinding, LiveCaptureContext, StoredSubagentSelection,
            SubagentDiscoveryLimits,
        };
        let f = Fixture::new();
        let context = LiveCaptureContext {
            libra_session_id: "claude__stored",
            provider_session_id: "stored-claude-native",
            verified_cwd: &f.cwd,
        };
        let expected = serde_json::json!({
            "cwd_base64": encode_helper_path(&f.cwd),
            "provider_session_id": context.provider_session_id,
            "byte_budget": 613, "source_limit": 2,
        });
        let response =
            encode_subagent_discovery_helper_success(SubagentDiscovery::default()).unwrap();
        let program = stored_request_helper(f._temp.path(), expected, response);
        let binding =
            LiveCaptureBinding::for_stored(super::super::observed_agents::AgentKind::ClaudeCode);
        let discovery = with_subagent_discovery_helper_program(
            program,
            binding.subagent_discovery().unwrap().discover_stored(
                &context,
                StoredSubagentSelection::ImportCurrent {
                    parent_locator: Some(Path::new("ignored-by-original-claude-layout")),
                },
                SubagentDiscoveryLimits {
                    byte_budget: 613,
                    source_limit: 2,
                },
                Instant::now() + Duration::from_secs(5),
            ),
        )
        .await
        .expect("Claude must keep original bounded protocol and allowances");
        assert_eq!(discovery.bytes_read, 0);
        assert!(!discovery.incomplete);
        assert!(discovery.warning.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_stored_expired_or_invalid_request_never_starts_helper() {
        use super::super::observed_agents::live_capture::{
            LiveCaptureContext, StoredSubagentSelection, SubagentDiscoveryLimits,
        };
        let f = Fixture::new();
        let context = LiveCaptureContext {
            libra_session_id: "codex__stored",
            provider_session_id: PARENT,
            verified_cwd: &f.cwd,
        };
        // Missing executable proves validation happens before spawn; no provider root is scanned.
        let missing = f._temp.path().join("must-not-execute");
        let error = with_subagent_discovery_helper_program(
            missing.clone(),
            discover_codex_subagent_contents_stored(
                &context,
                StoredSubagentSelection::TerminalRecoveryRoot,
                SubagentDiscoveryLimits {
                    byte_budget: 731,
                    source_limit: 1,
                },
                Instant::now() - Duration::from_secs(1),
            ),
        )
        .await
        .unwrap_err();
        assert!(error.downcast_ref::<SubagentDiscoveryDeadline>().is_some());
        let error = with_subagent_discovery_helper_program(
            missing,
            discover_codex_subagent_contents_stored(
                &context,
                StoredSubagentSelection::ImportCurrent {
                    parent_locator: Some(Path::new("relative/../untrusted")),
                },
                SubagentDiscoveryLimits {
                    byte_budget: 731,
                    source_limit: 1,
                },
                Instant::now() + Duration::from_secs(5),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid bounded locator"),
            "{error:#}"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_reads_bound_source_and_charges_control_io() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("DISCOVERED_CHILD_THINKING");
        std::fs::write(f.path(CHILD), &body).expect("child fixture");
        let query = f.query();
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert!(!discovery.incomplete);
        let source = &discovery.sources[0];
        assert_eq!(source.bytes, body);
        assert!(!source.partial);
        assert_eq!(
            source
                .codex_binding
                .as_ref()
                .and_then(|binding| binding.hook_turn.as_deref()),
            Some(TURN)
        );
        assert!(
            discovery.bytes_read > body.len() as u64,
            "headers/rechecks/parent reads must be charged"
        );
        let charged = discovery.bytes_read;
        let frame = encode_subagent_discovery_helper_success_with_query(discovery, Some(&query))
            .expect("bounded frame");
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(
            decode_subagent_discovery_helper_frame(frame.clone(), charged, 16, deadline).is_err()
        );
        let decoded = decode_subagent_discovery_helper_frame_with_query(
            frame,
            charged,
            16,
            deadline,
            Some((&query, &f.cwd)),
        )
        .expect("query-bound parent validation");
        assert_eq!(decoded.bytes_read, charged);
        assert_eq!(decoded.sources[0].bytes, body);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_keeps_large_native_base_instructions_header() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("THINKING_AFTER_LARGE_NATIVE_HEADER");
        let end = body
            .iter()
            .position(|b| *b == b'\n')
            .expect("header newline");
        let mut header: serde_json::Value = serde_json::from_slice(&body[..end]).expect("header");
        header["payload"]["base_instructions"] = serde_json::json!({"text": "x".repeat(80 * 1024)});
        let mut expanded = serde_json::to_vec(&header).expect("expanded native header");
        expanded.extend_from_slice(&body[end..]);
        std::fs::write(f.path(CHILD), &expanded).expect("large native header fixture");
        let discovery = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert_eq!(discovery.sources[0].bytes, expanded);
        assert!(!discovery.sources[0].partial);
        assert_eq!(
            discovery.sources[0]
                .codex_binding
                .as_ref()
                .and_then(|binding| binding.hook_turn.as_deref()),
            Some(TURN)
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_refuses_wrong_scope_root_and_child_before_body() {
        let f = Fixture::new();
        let mut query = f.query();
        if let CodexChildDiscoverySelection::Boundary { parent_path, .. } = &mut query.selection {
            *parent_path = None;
        }
        for (root, cwd, requested_child) in [
            (ROOT, f.cwd.join("other"), CHILD),
            ("other-root", f.cwd.clone(), CHILD),
            (ROOT, f.cwd.clone(), "other-child"),
        ] {
            let header = f.header(CHILD, root, Some(PARENT), &cwd);
            let mut body = header.clone();
            body.extend(vec![b'X'; 1024 * 1024]);
            std::fs::write(f.path(CHILD), body).expect("wrong-scope fixture");
            if let CodexChildDiscoverySelection::Boundary { child_id, .. } = &mut query.selection {
                *child_id = requested_child.into();
            }
            let mut budget = CodexDiscoveryBudget {
                spent: 0,
                cap: 2 * 1024 * 1024,
                deadline: Instant::now() + Duration::from_secs(2),
            };
            let result = read_codex_discovered_child(
                &f.pin(),
                &f.candidate(),
                &f.cwd,
                &query,
                None,
                &mut budget,
            );
            assert!(result.is_err() || result.expect("nonerror refusal").is_none());
            assert_eq!(
                budget.spent,
                header.len() as u64,
                "a rejected body must never be read"
            );
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_missing_parent_keeps_import_thinking_unassociated() {
        let f = Fixture::new();
        let body = f.child("ORPHAN_CHILD_ORDINARY_DATA");
        std::fs::write(f.path(CHILD), &body).expect("own-scope child");
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__parent-thread".into(),
            selection: CodexChildDiscoverySelection::Import {
                current: PARENT.into(),
                current_path: None,
            },
        };
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert!(discovery.incomplete);
        assert!(discovery.sources[0].partial);
        assert_eq!(discovery.sources[0].bytes, body);
        assert!(
            discovery.sources[0]
                .codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_import_rejects_proved_parent_root_contradiction() {
        let f = Fixture::new();
        f.parent();
        let header = f.header(CHILD, "contradictory-root", Some(PARENT), &f.cwd);
        let mut body = header.clone();
        body.extend(vec![b'X'; 1024 * 1024]);
        std::fs::write(f.path(CHILD), body).expect("contradictory fixture");
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__parent-thread".into(),
            selection: CodexChildDiscoverySelection::Import {
                current: PARENT.into(),
                current_path: None,
            },
        };
        let parent = CodexParentDiscoveryEvidence {
            header: CodexDiscoveryHeader {
                id: PARENT.into(),
                root: ROOT.into(),
                parent: None,
            },
            spawned: Default::default(),
            partial: false,
        };
        let mut budget = CodexDiscoveryBudget {
            spent: 0,
            cap: 2 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        let source = read_codex_discovered_child(
            &f.pin(),
            &f.candidate(),
            &f.cwd,
            &query,
            Some(&parent),
            &mut budget,
        )
        .expect("contradiction refusal");
        assert!(source.is_none());
        assert_eq!(budget.spent, header.len() as u64);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_refuses_symlink_files_and_directories_without_reads() {
        let f = Fixture::new();
        let outside = f.cwd.join("outside.jsonl");
        std::fs::write(&outside, f.child("NEVER_FOLLOW_THIS_BODY")).expect("outside fixture");
        std::os::unix::fs::symlink(&outside, f.path(CHILD)).expect("owned file link");
        let mut budget = CodexDiscoveryBudget {
            spent: 0,
            cap: TRANSCRIPT_READ_HARD_CAP_BYTES,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        assert!(
            read_codex_discovered_child(
                &f.pin(),
                &f.candidate(),
                &f.cwd,
                &f.query(),
                None,
                &mut budget
            )
            .is_err()
        );
        assert_eq!(budget.spent, 0);
        std::fs::remove_file(f.path(CHILD)).expect("remove functional symlink");
        std::fs::write(f.path(CHILD), f.child("POSITIVE_REAL_FILE")).expect("positive file");
        assert!(
            read_codex_discovered_child(
                &f.pin(),
                &f.candidate(),
                &f.cwd,
                &f.query(),
                None,
                &mut budget
            )
            .expect("positive regular file")
            .is_some()
        );
        let day = f.root.join("sessions/2026/10/20");
        let moved = f.cwd.join("moved-day");
        std::fs::rename(&day, &moved).expect("move functional directory");
        std::os::unix::fs::symlink(&moved, &day).expect("owned directory link");
        budget.spent = 0;
        assert!(
            read_codex_discovered_child(
                &f.pin(),
                &f.candidate(),
                &f.cwd,
                &f.query(),
                None,
                &mut budget
            )
            .is_err()
        );
        assert_eq!(budget.spent, 0);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_archive_prefix_uses_same_identity_and_longer_snapshot() {
        let f = Fixture::new();
        f.parent();
        let short = f.child("SHORT_READABLE_THINKING");
        let mut long = short.clone();
        long.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"text\":\"LATER_ORDINARY_BYTES\"}}\n");
        std::fs::write(f.path(CHILD), &short).expect("active fixture");
        std::fs::create_dir_all(f.root.join("archived_sessions")).expect("archive fixture");
        let archived = f
            .root
            .join("archived_sessions/rollout-local-child-thread.jsonl");
        std::fs::write(&archived, &long).expect("longer prefix fixture");
        let query = f.query();
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 1);
        assert_eq!(discovery.sources.len(), 1);
        assert_eq!(discovery.sources[0].bytes, long);
        assert!(!discovery.sources[0].partial);
        let key = discovery.sources[0].source_key.clone();
        std::fs::remove_file(f.path(CHILD)).expect("functional active removal");
        let replay = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(replay.sources.len(), 1);
        assert_eq!(replay.sources[0].source_key, key);
        assert_eq!(replay.sources[0].bytes, long);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_archive_conflict_retains_partial_without_turn_proof() {
        let f = Fixture::new();
        f.parent();
        let chosen = f.child("DIRECT_THINKING_RETAINED");
        let conflict = f.child("DIFFERENT_ARCHIVE_BYTES");
        std::fs::write(f.path(CHILD), &chosen).expect("direct fixture");
        std::fs::create_dir_all(f.root.join("archived_sessions")).expect("archive fixture");
        let archived = f
            .root
            .join("archived_sessions/rollout-local-child-thread.jsonl");
        std::fs::write(&archived, &conflict).expect("conflict fixture");
        let discovery = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert!(discovery.incomplete);
        assert_eq!(discovery.sources[0].bytes, chosen);
        assert!(discovery.sources[0].partial);
        assert!(
            discovery.sources[0]
                .codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
        assert_eq!(
            std::fs::read(&archived).expect("unchanged archive"),
            conflict
        );
        let second = f
            .root
            .join("sessions/2026/10/22/rollout-conflict-child-thread.jsonl");
        std::fs::create_dir_all(second.parent().expect("owned parent")).expect("second directory");
        std::fs::rename(&archived, &second).expect("move functional conflicting copy");
        let mut longer_prefix = chosen.clone();
        longer_prefix.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"text\":\"THIRD_PREFIX_MUST_NOT_ERASE_CONFLICT\"}}\n");
        let third = f
            .root
            .join("sessions/2026/10/21/rollout-other-child-thread.jsonl");
        std::fs::create_dir_all(third.parent().expect("owned parent")).expect("third directory");
        std::fs::write(third, &longer_prefix).expect("later prefix fixture");
        let again = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(again.sources.len(), 1);
        assert_eq!(again.sources[0].bytes, longer_prefix);
        assert!(
            again.sources[0].partial,
            "a later prefix cannot erase a previously observed conflict"
        );
        assert!(
            again.sources[0]
                .codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
        assert_eq!(
            std::fs::read(second).expect("conflicting copy unchanged"),
            conflict
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_boundary_uses_parent_header_without_reading_parent_body() {
        let f = Fixture::new();
        let header = f.header(PARENT, ROOT, None, &f.cwd);
        let mut parent = header.clone();
        parent.extend(vec![b'X'; 2 * 1024 * 1024]);
        std::fs::write(f.path(PARENT), parent).expect("unrelated parent body");
        let roots = f.pin();
        let candidate = codex_discovery_hint(&roots, &f.path(PARENT)).expect("parent candidate");
        let mut budget = CodexDiscoveryBudget {
            spent: 0,
            cap: 16 * 1024,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        let evidence = read_codex_parent_discovery_evidence(
            &roots,
            &candidate,
            &f.cwd,
            &f.query(),
            &mut budget,
        )
        .expect("only stable parent identity is needed");
        assert!(!evidence.partial);
        assert_eq!(
            budget.spent,
            2 * header.len() as u64,
            "the parent body is never read"
        );
        std::fs::write(f.path(CHILD), f.child("CHILD_TURN_SURVIVES_PARENT_BODY"))
            .expect("child fixture");
        let discovery = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert!(!discovery.sources[0].partial);
        assert_eq!(
            discovery.sources[0]
                .codex_binding
                .as_ref()
                .and_then(|binding| binding.hook_turn.as_deref()),
            Some(TURN)
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_foreign_legacy_header_is_not_an_incomplete_child() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("OURS_AMONG_FOREIGN_HISTORY");
        std::fs::write(f.path(CHILD), &body).expect("our child");
        let foreign = serde_json::json!({
            "type": "session_meta",
            "payload": { "id": "old-thread", "cwd": f.cwd.join("foreign-workspace") },
        });
        let mut bytes = serde_json::to_vec(&foreign).expect("foreign legacy header");
        bytes.push(b'\n');
        bytes.extend(vec![b'X'; 1024 * 1024]);
        std::fs::write(f.path("old-thread"), bytes).expect("foreign rollout");
        let query = CodexChildDiscoveryQuery {
            capture_session: "codex__root-thread".into(),
            selection: CodexChildDiscoverySelection::Recovery { root: ROOT.into() },
        };
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert_eq!(discovery.sources[0].bytes, body);
        assert!(!discovery.incomplete);
        assert_eq!(
            CodexForeignScope.to_string(),
            "Codex rollout belongs to a different workspace"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_same_scope_legacy_roots_are_excluded_without_body_reads() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("CHILD_NEXT_TO_LEGACY_ROOT_HISTORY");
        std::fs::write(f.path(CHILD), &body).expect("our child");
        let queries = [
            CodexChildDiscoveryQuery {
                capture_session: "codex__root-thread".into(),
                selection: CodexChildDiscoverySelection::Recovery { root: ROOT.into() },
            },
            CodexChildDiscoveryQuery {
                capture_session: "codex__parent-thread".into(),
                selection: CodexChildDiscoverySelection::Import {
                    current: PARENT.into(),
                    current_path: Some(f.path(PARENT).to_str().expect("fixture path").into()),
                },
            },
        ];
        let baselines = queries
            .iter()
            .map(|q| f.run(q, TRANSCRIPT_READ_HARD_CAP_BYTES, 16).bytes_read)
            .collect::<Vec<_>>();
        let mut header = serde_json::to_vec(&serde_json::json!({
            "type": "session_meta", "payload": { "id": "legacy-root", "cwd": f.cwd },
        }))
        .expect("legacy same-scope root");
        header.push(b'\n');
        let header_len = header.len() as u64;
        let mut legacy = header;
        legacy.extend(vec![b'X'; 1024 * 1024]);
        std::fs::write(f.path("legacy-root"), legacy).expect("owned legacy root history");
        for (query, baseline) in queries.iter().zip(baselines) {
            let discovery = f.run(query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
            assert_eq!(discovery.sources.len(), 1);
            assert_eq!(discovery.sources[0].bytes, body);
            assert!(!discovery.incomplete);
            assert_eq!(
                discovery.bytes_read - baseline,
                header_len,
                "legacy root body must not be read"
            );
        }
        assert_eq!(
            CodexNotChild.to_string(),
            "Codex rollout header does not describe a child thread"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_verified_spawn_without_content_never_reports_complete() {
        for import in [false, true] {
            for case in ["missing", "foreign", "metadata_only", "not_child"] {
                let f = Fixture::new();
                let parent = if import { PARENT } else { ROOT };
                f.parent_at(parent);
                let parent_bytes = std::fs::read(f.path(parent)).expect("owned parent");
                let parent_header_len = f.header(parent, ROOT, None, &f.cwd).len() as u64;
                let child_fee = match case {
                    "missing" => 0,
                    "foreign" => {
                        let header = f.header(CHILD, ROOT, Some(parent), &f.cwd.join("other"));
                        let fee = header.len() as u64;
                        let mut bytes = header;
                        bytes.extend(vec![b'X'; 1024 * 1024]);
                        std::fs::write(f.path(CHILD), bytes).expect("foreign spawned child");
                        fee
                    }
                    "metadata_only" => {
                        let header = f.header(CHILD, ROOT, Some(parent), &f.cwd);
                        // Probe, body header and final identity recheck each consume bytes.
                        let fee = 3 * header.len() as u64;
                        std::fs::write(f.path(CHILD), header).expect("metadata-only child");
                        fee
                    }
                    _ => {
                        let header = f.header(CHILD, ROOT, None, &f.cwd);
                        let fee = header.len() as u64;
                        let mut bytes = header;
                        bytes.extend(vec![b'X'; 1024 * 1024]);
                        std::fs::write(f.path(CHILD), bytes)
                            .expect("nonchild under an expected id");
                        fee
                    }
                };
                let path = Some(f.path(parent).to_str().expect("fixture path").to_string());
                let query = if import {
                    CodexChildDiscoveryQuery {
                        capture_session: format!("codex__{parent}"),
                        selection: CodexChildDiscoverySelection::Import {
                            current: PARENT.into(),
                            current_path: path,
                        },
                    }
                } else {
                    CodexChildDiscoveryQuery {
                        capture_session: format!("codex__{parent}"),
                        selection: CodexChildDiscoverySelection::Parent {
                            root: ROOT.into(),
                            current_path: path,
                        },
                    }
                };
                let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
                assert!(
                    discovery.incomplete,
                    "{import}/{case}: verified missing child must be reported"
                );
                assert!(discovery.sources.is_empty(), "{import}/{case}");
                assert_eq!(
                    discovery.bytes_read,
                    parent_bytes.len() as u64 + parent_header_len + child_fee,
                    "{import}/{case}: only an authorized child's body may be read"
                );
            }
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_child_discovery_unregistered_helper_is_unavailable_and_incomplete() {
        let f = Fixture::new();
        assert!(
            helper_program().is_none(),
            "library must not infer a helper"
        );
        let query = f.query();
        let discovery = discover_subagent_contents_bounded(
            &f.cwd,
            ROOT,
            Instant::now() + Duration::from_secs(1),
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            16,
            Some(&query),
        )
        .await
        .expect("unavailable provider discovery");
        assert!(discovery.incomplete);
        assert!(discovery.sources.is_empty());
        assert_eq!(discovery.bytes_read, 0);
        assert!(
            discovery
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("unavailable"))
        );
        let legacy = discover_claude_subagent_contents_bounded(
            &f.cwd,
            "legacy-parent",
            Instant::now() + Duration::from_secs(1),
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            16,
        )
        .await
        .expect("legacy unavailable remains compatible");
        assert!(!legacy.incomplete);
        assert_eq!(legacy.bytes_read, 0);
        assert!(legacy.sources.is_empty());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_validated_parent_does_not_exhaust_one_child_limit() {
        for import in [false, true] {
            let f = Fixture::new();
            let parent = if import { PARENT } else { ROOT };
            f.parent_at(parent);
            let body = f.child_of(parent, "CHILD_AT_ONE_SOURCE_LIMIT");
            std::fs::write(f.path(CHILD), &body).expect("owned active child");
            let archived = f.root.join("archived_sessions");
            std::fs::create_dir_all(&archived).expect("owned archive");
            let mut longer = body;
            longer.extend_from_slice(
                b"{\"type\":\"security_risk_score\",\"payload\":{\"scores\":{}}}\n",
            );
            std::fs::write(
                archived.join(f.path(CHILD).file_name().expect("child leaf")),
                &longer,
            )
            .expect("owned archived prefix");
            let path = Some(f.path(parent).to_str().expect("owned parent").to_string());
            let selection = if import {
                CodexChildDiscoverySelection::Import {
                    current: PARENT.into(),
                    current_path: path,
                }
            } else {
                CodexChildDiscoverySelection::Parent {
                    root: ROOT.into(),
                    current_path: path,
                }
            };
            let query = CodexChildDiscoveryQuery {
                capture_session: format!("codex__{parent}"),
                selection,
            };
            let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 1);
            assert_eq!(discovery.sources.len(), 1);
            assert_eq!(discovery.sources[0].bytes, longer);
            assert!(
                !discovery.incomplete,
                "a proved parent is not another candidate child"
            );
            assert!(!discovery.sources[0].partial);
            assert!(
                discovery.sources[0]
                    .codex_binding
                    .as_ref()
                    .is_some_and(|binding| binding.hook_turn.is_none())
            );
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_partial_descendant_evidence_does_not_degrade_complete_body() {
        for mode in ["parent", "import", "recovery"] {
            for case in ["large", "foreign_spawn", "unspawned_metadata_only"] {
                let f = Fixture::new();
                let parent = if mode == "import" { PARENT } else { ROOT };
                if case == "unspawned_metadata_only" {
                    std::fs::write(f.path(parent), f.header(parent, ROOT, None, &f.cwd))
                        .expect("complete parent without a recorded spawn");
                } else {
                    f.parent_at(parent);
                }
                let mut body = f.child_of(parent, "COMPLETE_BODY_WITH_BOUNDED_DESCENDANT_SCAN");
                match case {
                    "large" => {
                        let mut row = serde_json::to_vec(&serde_json::json!({
                            "type": "world_state", "payload": {"padding": "x".repeat(600_000)},
                        }))
                        .expect("bounded individual record");
                        row.push(b'\n');
                        body.extend_from_slice(&row);
                        body.extend_from_slice(&row);
                        assert!(body.len() > 1024 * 1024);
                    }
                    "foreign_spawn" => {
                        body.extend_from_slice(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"collab_agent_spawn_end\",\"sender_thread_id\":\"another-thread\",\"new_thread_id\":\"phantom-thread\"}}\n");
                    }
                    _ => body = f.header(CHILD, ROOT, Some(parent), &f.cwd),
                }
                std::fs::write(f.path(CHILD), &body).expect("complete child file");
                let path = Some(f.path(parent).to_str().expect("current parent path").into());
                let selection = match mode {
                    "parent" => CodexChildDiscoverySelection::Parent {
                        root: ROOT.into(),
                        current_path: path,
                    },
                    "import" => CodexChildDiscoverySelection::Import {
                        current: PARENT.into(),
                        current_path: path,
                    },
                    _ => CodexChildDiscoverySelection::Recovery { root: ROOT.into() },
                };
                let query = CodexChildDiscoveryQuery {
                    capture_session: format!("codex__{parent}"),
                    selection,
                };
                let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
                if case == "unspawned_metadata_only" {
                    assert!(discovery.sources.is_empty(), "{mode}/{case}");
                } else {
                    assert_eq!(discovery.sources.len(), 1, "{mode}/{case}");
                    assert_eq!(discovery.sources[0].bytes, body, "{mode}/{case}");
                    assert!(!discovery.sources[0].partial, "{mode}/{case}");
                }
                assert!(
                    !discovery.incomplete,
                    "{mode}/{case}: descendant evidence limits do not truncate ordinary body"
                );
                assert!(
                    !discovery
                        .usage_enumeration
                        .as_ref()
                        .expect("bounded enumeration")
                        .exhaustive,
                    "{mode}/{case}: incomplete descendant evidence cannot prove aggregate usage"
                );
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_partial_parent_without_children_remains_incomplete() {
        for import in [false, true] {
            for tail in [
                b"{\"type\":\"event_msg\"".as_slice(),
                b"malformed-parent-line\n".as_slice(),
            ] {
                let f = Fixture::new();
                let parent = if import { PARENT } else { ROOT };
                let mut bytes = f.header(parent, ROOT, None, &f.cwd);
                bytes.extend_from_slice(tail);
                std::fs::write(f.path(parent), bytes)
                    .expect("partial parent without visible spawns");
                let path = Some(f.path(parent).to_str().expect("owned path").to_string());
                let selection = if import {
                    CodexChildDiscoverySelection::Import {
                        current: PARENT.into(),
                        current_path: path,
                    }
                } else {
                    CodexChildDiscoverySelection::Parent {
                        root: ROOT.into(),
                        current_path: path,
                    }
                };
                let query = CodexChildDiscoveryQuery {
                    capture_session: format!("codex__{parent}"),
                    selection,
                };
                let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
                assert!(
                    discovery.incomplete,
                    "{import}: partial parent scan cannot prove absent children"
                );
                assert!(discovery.sources.is_empty());
                assert!(discovery.bytes_read > 0);
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_recent_child_precedes_old_large_header_history() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("RECENT_CHILD_FIRST_WITH_BOUNDED_BUDGET");
        std::fs::write(f.path(CHILD), &body).expect("recent child");
        let old = f.root.join("sessions/2020/01/01");
        std::fs::create_dir_all(&old).expect("old history directory");
        let mut header = serde_json::to_vec(&serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "old-foreign", "cwd": f.cwd.join("foreign"),
                "base_instructions": {"text": "x".repeat(80 * 1024)},
            },
        }))
        .expect("old large header");
        header.push(b'\n');
        for index in 0..260 {
            std::fs::write(
                old.join(format!("rollout-old-foreign-{index}.jsonl")),
                &header,
            )
            .expect("old functional rollout");
        }
        for selection in [
            CodexChildDiscoverySelection::Recovery { root: ROOT.into() },
            CodexChildDiscoverySelection::Import {
                current: PARENT.into(),
                current_path: Some(f.path(PARENT).to_str().expect("fixture path").into()),
            },
        ] {
            let query = CodexChildDiscoveryQuery {
                capture_session: "codex__root-thread".into(),
                selection,
            };
            let discovery = f.run(&query, 512 * 1024, 16);
            assert_eq!(
                discovery.sources.len(),
                1,
                "old header scans must not starve today's child"
            );
            assert_eq!(discovery.sources[0].bytes, body);
            assert!(discovery.bytes_read <= 512 * 1024);
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_unmapped_explicit_child_is_reported_incomplete() {
        let f = Fixture::new();
        f.parent();
        let outside = f.cwd.join("rollout-local-child-thread.jsonl");
        let body = f.child("OUTSIDE_HINT_GRANTS_NO_READ");
        std::fs::write(&outside, &body).expect("outside hint fixture");
        let mut query = f.query();
        if let CodexChildDiscoverySelection::Boundary { child_path, .. } = &mut query.selection {
            *child_path = outside.to_str().expect("fixture path").into();
        }
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert!(discovery.sources.is_empty());
        assert!(discovery.incomplete);
        std::fs::write(f.path(CHILD), &body).expect("authorized positive source");
        let fallback = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(fallback.sources.len(), 1);
        assert_eq!(fallback.sources[0].bytes, body);
        assert!(
            fallback.incomplete,
            "an unmapped explicit locator remains diagnosed"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_expired_absolute_clock_and_old_helper_never_acknowledge_success() {
        let f = Fixture::new();
        let query = f.query();
        let request = SubagentDiscoveryHelperRequest {
            cwd_base64: encode_helper_path(&f.cwd),
            provider_session_id: ROOT.into(),
            remaining_ms: 5000,
            byte_budget: TRANSCRIPT_READ_HARD_CAP_BYTES,
            source_limit: 16,
            codex_query: Some(query.clone()),
            codex_deadline_unix_ms: Some(0),
        };
        let output =
            run_subagent_discovery_helper(&serde_json::to_vec(&request).expect("expired request"))
                .expect("safe deadline frame");
        let result = decode_subagent_discovery_helper_frame_with_query(
            output,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            16,
            Instant::now() + Duration::from_secs(1),
            Some((&query, &f.cwd)),
        )
        .expect_err("expired absolute bound cannot perform discovery");
        assert!(result.is::<SubagentDiscoveryDeadline>());
        let legacy = encode_subagent_discovery_helper_success(SubagentDiscovery::default())
            .expect("old empty helper reply");
        assert!(
            decode_subagent_discovery_helper_frame_with_query(
                legacy,
                TRANSCRIPT_READ_HARD_CAP_BYTES,
                16,
                Instant::now() + Duration::from_secs(1),
                Some((&query, &f.cwd)),
            )
            .is_err(),
            "an old helper cannot silently return complete with zero Codex sources"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_child_discovery_producer_reserves_reply_time_after_spawn() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let helper = f.cwd.join("functional-clock-helper.py");
        std::fs::write(
            &helper,
            r#"#!/usr/bin/env python3
import json,struct,sys,time
time.sleep(0.12)
request=json.load(sys.stdin)
assert request['codex_query']['selection']['mode']=='boundary'
assert 0 < request['remaining_ms'] <= 350
assert request['codex_deadline_unix_ms'] <= int(time.time()*1000)+request['remaining_ms']
header=json.dumps({'status':'error','deadline_exceeded':True},separators=(',',':')).encode()
sys.stdout.buffer.write(struct.pack('<I',len(header))+header)
"#,
        )
        .expect("owned private helper clock fixture");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
            .expect("functional helper executable");
        let query = f.query();
        let deadline = Instant::now() + Duration::from_millis(700);
        let result = with_subagent_discovery_helper_program(helper, async {
            discover_subagent_contents_bounded(
                &f.cwd,
                ROOT,
                deadline,
                TRANSCRIPT_READ_HARD_CAP_BYTES,
                16,
                Some(&query),
            )
            .await
        })
        .await;
        let error = result.expect_err("bounded helper deadline reply");
        assert!(error.is::<SubagentDiscoveryDeadline>(), "{error:#}");
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_direct_hint_survives_directory_entry_limit() {
        let f = Fixture::new();
        f.parent();
        let body = f.child("DIRECT_HINT_MUST_NOT_BE_STARVED");
        std::fs::write(f.path(CHILD), &body).expect("direct fixture");
        for index in 0..4200 {
            std::fs::write(
                f.root
                    .join("sessions/2026/10/20")
                    .join(format!("irrelevant-{index}")),
                b"",
            )
            .expect("owned directory-limit fixture");
        }
        let discovery = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert!(discovery.incomplete);
        assert_eq!(discovery.sources.len(), 1);
        assert_eq!(discovery.sources[0].bytes, body);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_budget_and_source_limit_zero_do_no_provider_reads() {
        let f = Fixture::new();
        f.parent();
        std::fs::write(f.path(CHILD), f.child("BUDGETED_THINKING")).expect("child fixture");
        for (cap, limit) in [(0, 16), (TRANSCRIPT_READ_HARD_CAP_BYTES, 0), (512, 16)] {
            let discovery = f.run(&f.query(), cap, limit);
            assert!(discovery.incomplete);
            assert!(discovery.bytes_read <= cap);
            if cap == 0 || limit == 0 {
                assert_eq!(discovery.bytes_read, 0);
                assert!(discovery.sources.is_empty());
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_bad_tail_is_retained_with_malformed_count() {
        let f = Fixture::new();
        f.parent();
        let mut body = f.child("THINKING_BEFORE_BAD_TAIL");
        body.extend_from_slice(b"{\"unfinished\":\"retain this");
        std::fs::write(f.path(CHILD), &body).expect("partial child");
        let discovery = f.run(&f.query(), TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        assert_eq!(discovery.sources.len(), 1);
        assert_eq!(discovery.sources[0].bytes, body);
        assert_eq!(discovery.sources[0].malformed_lines, 1);
        assert!(discovery.sources[0].partial);
        assert!(
            discovery.sources[0]
                .codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_discovery_wire_rejects_scope_identity_and_fee_forgery() {
        let f = Fixture::new();
        f.parent();
        std::fs::write(f.path(CHILD), f.child("WIRE_THINKING")).expect("child fixture");
        let query = f.query();
        let discovery = f.run(&query, TRANSCRIPT_READ_HARD_CAP_BYTES, 16);
        let charged = discovery.bytes_read;
        let frame =
            encode_subagent_discovery_helper_success_with_query(discovery.clone(), Some(&query))
                .expect("valid frame");
        let decode =
            |frame: Vec<u8>, budget: u64, limit: usize, query: &CodexChildDiscoveryQuery| {
                decode_subagent_discovery_helper_frame_with_query(
                    frame,
                    budget,
                    limit,
                    Instant::now() + Duration::from_secs(2),
                    Some((query, &f.cwd)),
                )
            };
        assert!(decode(frame.clone(), charged - 1, 16, &query).is_err());
        assert!(decode(frame.clone(), charged, 0, &query).is_err());
        let mut wrong_query = query.clone();
        wrong_query.capture_session = "codex__different".into();
        assert!(decode(frame.clone(), charged, 16, &wrong_query).is_err());
        let header_len = u32::from_le_bytes(frame[..4].try_into().expect("header length")) as usize;
        let header_end = 4 + header_len;
        for (field, value) in [
            ("metadata_bytes_read", serde_json::json!(charged)),
            ("bytes_read", serde_json::json!(0)),
        ] {
            let mut header: serde_json::Value =
                serde_json::from_slice(&frame[4..header_end]).expect("header");
            header[field] = value;
            let header = serde_json::to_vec(&header).expect("mutated control");
            let mut forged = (header.len() as u32).to_le_bytes().to_vec();
            forged.extend(header);
            forged.extend_from_slice(&frame[header_end..]);
            assert!(decode(forged, charged, 16, &query).is_err(), "{field}");
        }
        let original_header: serde_json::Value =
            serde_json::from_slice(&frame[4..header_end]).expect("original header");
        let mut empty = original_header.clone();
        empty["sources"] = serde_json::json!([]);
        empty["bytes_read"] = serde_json::json!(0);
        empty["metadata_bytes_read"] = serde_json::json!(0);
        empty["incomplete"] = serde_json::json!(false);
        let empty = serde_json::to_vec(&empty).expect("forged empty success");
        let mut forged = (empty.len() as u32).to_le_bytes().to_vec();
        forged.extend(empty);
        assert!(
            decode(forged, charged, 16, &query)
                .expect_err("explicit child cannot be silently absent")
                .to_string()
                .contains("omitted an explicitly requested source")
        );
        let mut duplicate = original_header;
        let source_header = duplicate["sources"][0].clone();
        duplicate["sources"] = serde_json::json!([source_header.clone(), source_header]);
        duplicate["bytes_read"] =
            serde_json::json!(charged + discovery.sources[0].bytes.len() as u64);
        let duplicate = serde_json::to_vec(&duplicate).expect("duplicate header");
        let mut forged = (duplicate.len() as u32).to_le_bytes().to_vec();
        forged.extend(duplicate);
        forged.extend_from_slice(&frame[header_end..]);
        forged.extend_from_slice(&frame[header_end..]);
        assert!(
            decode(forged, TRANSCRIPT_READ_HARD_CAP_BYTES, 16, &query)
                .expect_err("duplicate logical source must not be accepted")
                .to_string()
                .contains("duplicate logical sources")
        );
        let mut wrong_scope = discovery;
        let old = &wrong_scope.sources[0].bytes;
        let end = old
            .iter()
            .position(|b| *b == b'\n')
            .expect("header terminator");
        let mut header: serde_json::Value =
            serde_json::from_slice(&old[..end]).expect("source header");
        header["payload"]["cwd"] = serde_json::to_value(f.cwd.join("other")).expect("wrong cwd");
        let mut changed = serde_json::to_vec(&header).expect("changed header");
        changed.extend_from_slice(&old[end..]);
        wrong_scope.bytes_read = wrong_scope.bytes_read - old.len() as u64 + changed.len() as u64;
        wrong_scope.sources[0].bytes = changed;
        let frame = encode_subagent_discovery_helper_success_with_query(wrong_scope, Some(&query))
            .expect("control alone cannot prove cwd");
        assert!(decode(frame, TRANSCRIPT_READ_HARD_CAP_BYTES, 16, &query).is_err());
    }
}

#[cfg(all(test, feature = "test-live-agent"))]
mod codex_child_content_local_tests {
    use super::*;

    const TURN: &str = "01991d45-6f00-7000-8000-000000000001";
    const LATER: &str = "01991d45-6f00-7000-8000-000000000002";

    fn child_source(turn: Option<&str>, tail: &[u8]) -> DiscoveredSubagentContent {
        let mut source = DiscoveredSubagentContent::fixture(
            "codex",
            "source/sha256/local-child",
            b"",
            Some("child-thread"),
        );
        source.codex_binding = Some(CodexChildBinding {
            root: "root-thread".into(),
            parent: "parent-thread".into(),
            child: "child-thread".into(),
            capture_session: "codex__parent-thread".into(),
            hook_turn: turn.map(str::to_owned),
        });
        source.bytes = br#"{"type":"session_meta","payload":{"id":"child-thread","session_id":"root-thread","parent_thread_id":"parent-thread","cli_version":"0.162.1"}}
"#.to_vec();
        if let Some(turn) = turn {
            source.bytes.extend_from_slice(
                format!("{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{turn}\"}}}}\n")
                    .as_bytes(),
            );
        }
        source.bytes.extend_from_slice(tail);
        source
    }

    fn request(source: &DiscoveredSubagentContent) -> Vec<u8> {
        let mut bytes = encode_subagent_projection_request_header(source).expect("bounded request");
        bytes.extend_from_slice(&source.bytes);
        bytes
    }

    fn project(source: &DiscoveredSubagentContent) -> SafeSubagentProjection {
        let frame = run_subagent_projection_helper(request(source)).expect("projection worker");
        decode_subagent_projection_frame(frame, source.bytes.len() as u64)
            .expect("bounded ordinary projection")
    }

    fn thinking() -> &'static [u8] {
        br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"CHILD_THINKING_READABLE"}],"encrypted_content":"opaque-child-value","future_field":{"nested":"keep-this"}}}
"#
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_error_progress_discovery_role_does_not_change_body_identity() {
        let complete = child_source(Some(TURN), thinking());
        let mut partial = complete.clone();
        partial.partial = true;
        partial.codex_binding.as_mut().unwrap().hook_turn = None;
        let a = project(&complete);
        let b = project(&partial);
        assert!(
            !a.partial && b.partial,
            "fixture must genuinely change the role assessment"
        );
        assert_eq!(a.transcript.as_ref(), b.transcript.as_ref());
        assert!(a.codex_binding.as_ref().unwrap().hook_turn.is_some());
        assert!(b.codex_binding.as_ref().unwrap().hook_turn.is_none());
        assert_eq!(
            a.content_digest, b.content_digest,
            "identical saved bytes cannot gain another revision just from live/recovery completeness"
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_retains_plaintext_opaque_and_unknown_fields() {
        let source = child_source(Some(TURN), thinking());
        let safe = project(&source);
        assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
        assert_eq!(safe.turn_count, 1);
        assert!(!safe.partial, "ordinary source should remain complete");
        assert!(safe.codex_binding.as_ref().is_some_and(|binding| {
            binding.hook_turn.as_deref() == Some(TURN)
                && binding.capture_session == "codex__parent-thread"
        }));
        assert_eq!(
            safe.content_digest,
            subagent_content_identity_digest_bytes(&safe.transcript, false, 0),
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_reasoning_only_is_not_an_empty_shell() {
        let source = child_source(None, thinking());
        let safe = project(&source);
        assert_eq!(safe.turn_count, 0);
        assert!(!safe.transcript.is_empty());
        assert!(
            String::from_utf8_lossy(safe.transcript.as_ref()).contains("CHILD_THINKING_READABLE")
        );
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );

        let empty = child_source(Some(TURN), b"");
        assert_eq!(
            run_subagent_projection_helper(request(&empty)).expect("safe worker failure"),
            vec![SUBAGENT_PROJECTION_FAILED],
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_scans_escaped_credentials_without_losing_thinking() {
        let source = child_source(Some(TURN), br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"safe thinking \u0041KIAABCDEFGHIJKLMNOP"}],"encrypted_content":"opaque-value"}}
"#);
        let safe = project(&source);
        let text = String::from_utf8_lossy(safe.transcript.as_ref());
        assert!(text.contains("safe thinking"), "{text}");
        assert!(text.contains("opaque-value"));
        assert!(!text.contains(r"\u0041KIAABCDEFGHIJKLMNOP"));
        assert!(!text.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(!text.contains("ABCDEFGHIJKLMNOP"));
        let report: RedactionReport =
            serde_json::from_value(safe.report_value).expect("safe report");
        assert!(report.match_count() > 0, "decoded token must be scanned");
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_new_start_invalidates_previous_turn_until_context() {
        for kind in ["task_started", "turn_started"] {
            let start = |turn: &str| {
                format!(
                    "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\",\"turn_id\":\"{turn}\"}}}}\n"
                )
            };
            let mut first = child_source(None, start(TURN).as_bytes());
            first.bytes.extend_from_slice(
                format!("{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{TURN}\"}}}}\n")
                    .as_bytes(),
            );
            first.bytes.extend_from_slice(thinking());
            first.codex_binding.as_mut().expect("binding").hook_turn = Some(TURN.into());
            let safe = project(&first);
            assert!(
                !safe.partial,
                "normal startup must not poison the projection"
            );
            assert_eq!(
                safe.codex_binding
                    .as_ref()
                    .expect("binding")
                    .hook_turn
                    .as_deref(),
                Some(TURN)
            );

            first.bytes.extend_from_slice(start(LATER).as_bytes());
            let safe = project(&first);
            assert_eq!(safe.transcript.as_ref(), first.bytes.as_slice());
            assert!(
                safe.codex_binding
                    .as_ref()
                    .expect("binding")
                    .hook_turn
                    .is_none(),
                "a new start cannot reuse the prior context"
            );
            first.codex_binding.as_mut().expect("binding").hook_turn = Some(LATER.into());
            assert!(
                project(&first)
                    .codex_binding
                    .as_ref()
                    .expect("binding")
                    .hook_turn
                    .is_none(),
                "a start alone cannot establish the new context"
            );

            first.bytes.extend_from_slice(
                format!("{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{LATER}\"}}}}\n")
                    .as_bytes(),
            );
            first.bytes.extend_from_slice(thinking());
            let safe = project(&first);
            assert!(!safe.partial);
            assert_eq!(safe.turn_count, 2);
            assert_eq!(
                safe.codex_binding
                    .as_ref()
                    .expect("binding")
                    .hook_turn
                    .as_deref(),
                Some(LATER)
            );
            assert_eq!(safe.transcript.as_ref(), first.bytes.as_slice());

            for invalid in [
                serde_json::Value::Null,
                serde_json::json!(""),
                serde_json::json!(7),
            ] {
                let mut source = child_source(Some(TURN), thinking());
                source.bytes.extend(
                    serde_json::to_vec(&serde_json::json!({
                        "type": "event_msg", "payload": {"type": kind, "turn_id": invalid},
                    }))
                    .expect("start marker"),
                );
                source.bytes.push(b'\n');
                let safe = project(&source);
                assert!(safe.partial);
                assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
                assert!(
                    safe.codex_binding
                        .as_ref()
                        .expect("binding")
                        .hook_turn
                        .is_none()
                );
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_does_not_require_future_task_complete() {
        let source = child_source(Some(TURN), thinking());
        let safe = project(&source);
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.as_deref() == Some(TURN))
        );
        let mut late = source;
        late.bytes.extend_from_slice(
            format!("{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{LATER}\"}}}}\n")
                .as_bytes(),
        );
        late.bytes.extend_from_slice(thinking());
        let safe = project(&late);
        assert_eq!(safe.turn_count, 2);
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
        assert!(String::from_utf8_lossy(safe.transcript.as_ref()).contains(LATER));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_usage_before_stop_preserves_observed_turn() {
        let mut source = child_source(Some(TURN), thinking());
        source.bytes.extend_from_slice(
            format!(
                "{{\"type\":\"token_usage_record\",\"payload\":{{\"thread_id\":\"child-thread\",\"turn_id\":\"{TURN}\",\"session_id\":\"root-thread\",\"root_turn_id\":\"{LATER}\",\"response_id\":\"response-1\",\"usage\":{{\"reasoning_output_tokens\":7}},\"turn_token_usage\":{{}},\"thread_token_usage\":{{}}}}}}\n"
            )
            .as_bytes(),
        );
        let safe = project(&source);
        assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
        assert!(
            !safe.partial,
            "completed response usage must not invalidate Stop"
        );
        assert_eq!(safe.turn_count, 1);
        assert_eq!(
            safe.codex_binding
                .as_ref()
                .and_then(|binding| binding.hook_turn.as_deref()),
            Some(TURN),
            "root_turn_id belongs to the root and must not replace the child turn",
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_known_snapshots_keep_bytes_without_new_turns() {
        let mut source = child_source(Some(TURN), thinking());
        source.bytes.extend_from_slice(br#"{"type":"world_state","payload":{"full":true,"state":{"ordinary_snapshot":"KEEP_WORLD_STATE","future":{"opaque":"keep"}}}}
{"type":"security_risk_score","payload":{"scores":{"tool":0.25},"call_id":"call-1","action":{"ordinary_action":"KEEP_ACTION"}}}
"#);
        let safe = project(&source);
        assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
        assert!(!safe.partial);
        assert_eq!(safe.turn_count, 1);
        assert_eq!(
            safe.codex_binding
                .as_ref()
                .and_then(|binding| binding.hook_turn.as_deref()),
            Some(TURN),
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_metadata_contradictions_keep_content_unassociated() {
        let usage = serde_json::json!({
            "type": "token_usage_record",
            "payload": {
                "thread_id": "child-thread", "session_id": "root-thread",
                "turn_id": TURN, "root_turn_id": LATER,
                "usage": {"reasoning_output_tokens": 7},
            },
        });
        for (field, replacement) in [
            ("thread_id", serde_json::json!("other-child")),
            ("session_id", serde_json::json!("other-root")),
            ("turn_id", serde_json::json!(LATER)),
            ("thread_id", serde_json::Value::Null),
            ("session_id", serde_json::Value::Null),
            ("turn_id", serde_json::Value::Null),
            ("thread_id", serde_json::json!({"untrusted": "object"})),
            ("thread_id", serde_json::json!("x".repeat(129))),
        ] {
            let mut record = usage.clone();
            record["payload"][field] = replacement;
            let mut source = child_source(Some(TURN), thinking());
            source
                .bytes
                .extend(serde_json::to_vec(&record).expect("fixture record"));
            source.bytes.push(b'\n');
            let safe = project(&source);
            assert!(safe.partial, "contradictory {field} must be partial");
            assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
            assert!(
                safe.codex_binding
                    .as_ref()
                    .is_some_and(|binding| binding.hook_turn.is_none())
            );
        }
        for kind in ["world_state", "security_risk_score"] {
            for (field, value) in [
                ("thread_id", "other-child"),
                ("session_id", "other-root"),
                ("turn_id", LATER),
            ] {
                let mut source = child_source(Some(TURN), thinking());
                source.bytes.extend_from_slice(
                    format!("{{\"type\":\"{kind}\",\"payload\":{{\"{field}\":\"{value}\"}}}}\n")
                        .as_bytes(),
                );
                let safe = project(&source);
                assert!(safe.partial, "{kind}/{field} must be partial");
                assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
                assert!(
                    safe.codex_binding
                        .as_ref()
                        .is_some_and(|binding| binding.hook_turn.is_none())
                );
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_metadata_cannot_start_turn_or_replace_content() {
        let records = format!(
            "{{\"type\":\"token_usage_record\",\"payload\":{{\"thread_id\":\"child-thread\",\"session_id\":\"root-thread\",\"turn_id\":\"{TURN}\"}}}}\n{{\"type\":\"world_state\",\"payload\":{{\"full\":false,\"state\":{{}}}}}}\n{{\"type\":\"security_risk_score\",\"payload\":{{\"scores\":{{}}}}}}\n"
        );
        let source = child_source(Some(TURN), records.as_bytes());
        assert_eq!(
            run_subagent_projection_helper(request(&source)).expect("safe empty result"),
            vec![SUBAGENT_PROJECTION_FAILED]
        );
        let mut source = child_source(None, thinking());
        source.bytes.extend_from_slice(records.as_bytes());
        let safe = project(&source);
        assert!(safe.partial);
        assert_eq!(safe.turn_count, 0);
        assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_context_and_future_kinds_remain_partial() {
        for kind in [
            "compacted",
            "retained_context",
            "inter_agent_communication",
            "inter_agent_communication_metadata",
            "realtime_item",
            "future_metadata",
        ] {
            let mut source = child_source(Some(TURN), thinking());
            source.bytes.extend_from_slice(format!(
                "{{\"type\":\"{kind}\",\"payload\":{{\"ordinary_text\":\"KEEP_CONTEXT_BODY\"}}}}\n"
            ).as_bytes());
            let safe = project(&source);
            assert!(
                safe.partial,
                "{kind} must not be treated as a neutral snapshot"
            );
            assert_eq!(safe.transcript.as_ref(), source.bytes.as_slice());
            assert!(
                safe.codex_binding
                    .as_ref()
                    .is_some_and(|binding| binding.hook_turn.is_none())
            );
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_counter_and_contradictory_end_are_unassociated() {
        let source = child_source(Some("auto-compact-1"), thinking());
        let safe = project(&source);
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
        let mut source = child_source(Some(TURN), thinking());
        source.bytes.extend_from_slice(format!(
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"{LATER}\"}}}}\n"
        ).as_bytes());
        let safe = project(&source);
        assert!(safe.partial);
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );
        assert!(
            String::from_utf8_lossy(safe.transcript.as_ref()).contains("CHILD_THINKING_READABLE")
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_keeps_partial_tail_without_association() {
        let mut source = child_source(Some(TURN), thinking());
        source.bytes.extend_from_slice(
            br#"{"type":"response_item","payload":{"type":"reasoning","text":"unfinished thinking"#,
        );
        let safe = project(&source);
        assert!(safe.partial);
        let text = String::from_utf8_lossy(safe.transcript.as_ref());
        assert!(text.contains("CHILD_THINKING_READABLE"));
        assert!(text.contains("unfinished thinking"));
        assert!(
            safe.codex_binding
                .as_ref()
                .is_some_and(|binding| binding.hook_turn.is_none())
        );

        let source = child_source(Some(TURN),
            br#"{"type":"future_content","payload":{"unrecognized_thinking":"retained-future-body"}}
"#);
        let safe = project(&source);
        assert!(safe.partial);
        assert!(String::from_utf8_lossy(safe.transcript.as_ref()).contains("retained-future-body"));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_rejects_wrong_header_and_duplicate_identity() {
        let source = child_source(Some(TURN), thinking());
        let mut wrong = source.clone();
        wrong.bytes = String::from_utf8(wrong.bytes)
            .expect("fixture UTF-8")
            .replacen("\"id\":\"child-thread\"", "\"id\":\"other-thread\"", 1)
            .into_bytes();
        assert_eq!(
            run_subagent_projection_helper(request(&wrong)).expect("safe error"),
            vec![1]
        );
        let mut duplicate = source;
        duplicate.bytes = String::from_utf8(duplicate.bytes)
            .expect("fixture UTF-8")
            .replacen(
                "\"id\":\"child-thread\"",
                "\"id\":\"child-thread\",\"id\":\"other-thread\"",
                1,
            )
            .into_bytes();
        assert_eq!(
            run_subagent_projection_helper(request(&duplicate)).expect("safe error"),
            vec![1]
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_protocol_preserves_legacy_and_bounds_metadata() {
        let mut old = DiscoveredSubagentContent::fixture("claude_code", "source/old", b"old", None);
        old.partial = true;
        old.malformed_lines = 2;
        let header = encode_subagent_projection_request_header(&old).expect("legacy header");
        assert_eq!(header, vec![1, 2, 0, 0, 0, 0, 0, 0, 0]);
        let decoded = decode_subagent_projection_request(request(&old)).expect("legacy request");
        assert_eq!(decoded.provider_kind, "claude_code");
        assert!(decoded.codex_binding.is_none());
        assert_eq!(decoded.bytes, b"old");

        let source = child_source(Some(TURN), thinking());
        let original = request(&source);
        let decoded = decode_subagent_projection_request(original.clone()).expect("Codex header");
        assert_eq!(decoded.bytes, source.bytes);
        assert!(decoded.codex_binding == source.codex_binding);
        // Tags 4/5 now carry the bounded optional usage work boundary.
        for tag in [6_u8, 127, 255] {
            let mut invalid = original.clone();
            invalid[0] = tag;
            assert!(decode_subagent_projection_request(invalid).is_none());
        }
        let mut too_big = original.clone();
        too_big[9..11].copy_from_slice(&513_u16.to_le_bytes());
        assert!(decode_subagent_projection_request(too_big).is_none());
        let mut missing = original;
        missing[9..11].copy_from_slice(&0_u16.to_le_bytes());
        assert!(decode_subagent_projection_request(missing).is_none());

        let mut bad = source;
        bad.stable_subagent_id = Some("different-child".into());
        assert!(encode_subagent_projection_request_header(&bad).is_err());
        assert!(
            CodexChildBinding::decode(br#"{"PRIVATE_METADATA_CANARY":"value"}"#)
                .err()
                .is_some_and(|error| !format!("{error:#}").contains("PRIVATE_METADATA_CANARY"))
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_response_rejects_truncated_or_unknown_frames() {
        let source = child_source(Some(TURN), thinking());
        let valid = run_subagent_projection_helper(request(&source)).expect("worker");
        assert_eq!(valid[0], SUBAGENT_PROJECTION_CODEX_COMPLETE);
        for end in [0, 1, 53, 54, 55, valid.len() - 1] {
            assert!(
                decode_subagent_projection_frame(valid[..end].to_vec(), source.bytes.len() as u64)
                    .is_err()
            );
        }
        let mut extra = valid.clone();
        extra.push(0);
        assert!(decode_subagent_projection_frame(extra, source.bytes.len() as u64).is_err());
        let mut invalid = valid;
        invalid[0] = 127;
        assert!(decode_subagent_projection_frame(invalid, source.bytes.len() as u64).is_err());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_reply_cannot_change_capture_or_promote_import() {
        let source = child_source(Some(TURN), thinking());
        let mut safe = project(&source);
        validate_subagent_projection_binding(&source, &safe).expect("matching response");
        safe.codex_binding
            .as_mut()
            .expect("Codex metadata")
            .capture_session = "codex__other".into();
        assert!(validate_subagent_projection_binding(&source, &safe).is_err());

        let source = child_source(None, thinking());
        let mut safe = project(&source);
        safe.codex_binding
            .as_mut()
            .expect("Codex metadata")
            .hook_turn = Some(TURN.into());
        assert!(validate_subagent_projection_binding(&source, &safe).is_err());
        let old = DiscoveredSubagentContent::fixture("claude_code", "source/old", b"old", None);
        assert!(validate_subagent_projection_binding(&old, &safe).is_err());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_content_projection_oversize_is_rejected_before_normalization() {
        let mut source = child_source(Some(TURN), thinking());
        source.bytes = vec![b'x'; TRANSCRIPT_READ_HARD_CAP_BYTES as usize + 1];
        assert_eq!(
            run_subagent_projection_helper(request(&source)).expect("bounded safe failure"),
            vec![SUBAGENT_PROJECTION_FAILED],
        );
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_child_usage_projection_local_tests {
    use super::*;

    const ROOT: &str = "123e4567-e89b-12d3-a456-426614174031";
    const CHILD: &str = "123e4567-e89b-12d3-a456-426614174099";
    const TURN: &str = "01991d45-6f00-7000-8000-000000000001";

    fn source() -> DiscoveredSubagentContent {
        let usage = serde_json::json!({"input_tokens":3,"output_tokens":2,
            "cached_input_tokens":1,"cache_write_input_tokens":0,
            "reasoning_output_tokens":1,"total_tokens":5});
        let rows = [
            serde_json::json!({"type":"session_meta","payload":{"id":CHILD,"session_id":ROOT,
                "parent_thread_id":ROOT,"cli_version":"0.162.1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":TURN}}),
            serde_json::json!({"type":"turn_context","payload":{"turn_id":TURN}}),
            serde_json::json!({"type":"response_item","payload":{"type":"reasoning",
                "summary":[{"type":"summary_text","text":"CX26-KEPT-ORDINARY-THINKING"}]}}),
            serde_json::json!({"type":"token_usage_record","payload":{"thread_id":CHILD,
                "session_id":ROOT,"turn_id":TURN,"root_turn_id":TURN,"response_id":"cx26-response",
                "usage":usage,"turn_token_usage":usage,"thread_token_usage":usage}}),
        ];
        let bytes = (rows
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .into_bytes();
        DiscoveredSubagentContent::fixture_codex_child(ROOT, ROOT, CHILD, "codex__parent", &bytes)
    }

    fn frame(source: &DiscoveredSubagentContent, not_after: Option<i64>) -> Vec<u8> {
        let mut bytes = encode_subagent_projection_work_request_header(source, not_after).unwrap();
        bytes.extend_from_slice(&source.bytes);
        run_subagent_projection_helper(bytes).unwrap()
    }

    fn decode(frame: Vec<u8>, source: &DiscoveredSubagentContent) -> SafeSubagentProjection {
        decode_subagent_projection_frame(frame, source.bytes.len() as u64).unwrap()
    }

    fn replace_usage(frame: &[u8], usage: &[u8]) -> Vec<u8> {
        assert_eq!(frame[0], SUBAGENT_PROJECTION_CODEX_USAGE_COMPLETE);
        let binding_len = u16::from_le_bytes(frame[54..56].try_into().unwrap()) as usize;
        let length_at = 56 + binding_len;
        let old_len =
            u16::from_le_bytes(frame[length_at..length_at + 2].try_into().unwrap()) as usize;
        let mut changed = frame[..length_at].to_vec();
        changed.extend_from_slice(&(usage.len() as u16).to_le_bytes());
        changed.extend_from_slice(usage);
        changed.extend_from_slice(&frame[length_at + 2 + old_len..]);
        changed
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_new_request_round_trips_absolute_work_boundary_and_legacy_request() {
        let source = source();
        for partial in [false, true] {
            let mut source = source.clone();
            source.partial = partial;
            let mut request =
                encode_subagent_projection_work_request_header(&source, Some(1234)).unwrap();
            assert_eq!(request[0], if partial { 5 } else { 4 });
            request.extend_from_slice(&source.bytes);
            let (decoded, deadline) = decode_subagent_projection_work_request(request).unwrap();
            assert_eq!(deadline, Some(1234));
            assert_eq!(decoded.bytes, source.bytes);
            assert!(decoded.codex_binding == source.codex_binding);
            assert_eq!(decoded.partial, partial);
        }
        let mut request = encode_subagent_projection_request_header(&source).unwrap();
        assert_eq!(request[0], 2);
        request.extend_from_slice(&source.bytes);
        assert_eq!(
            decode_subagent_projection_work_request(request).unwrap().1,
            None
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_separate_segment_preserves_ordinary_digest_and_redaction_report() {
        let source = source();
        let legacy_frame = frame(&source, None);
        let usage_frame = frame(&source, Some(Utc::now().timestamp_millis() + 10_000));
        assert_eq!(legacy_frame[0], 2);
        assert_eq!(usage_frame[0], 3);
        let legacy = decode(legacy_frame, &source);
        let current = decode(usage_frame, &source);
        assert_eq!(legacy.transcript, current.transcript);
        assert_eq!(legacy.content_digest, current.content_digest);
        assert_eq!(legacy.report_value, current.report_value);
        assert!(legacy.codex_usage.is_none());
        let usage = current.codex_usage.unwrap();
        assert_eq!(usage.usage_state, CodexUsageState::Complete);
        assert_eq!(usage.token_usage.unwrap().input_tokens, 3);
        assert!(
            String::from_utf8_lossy(current.transcript.as_ref())
                .contains("CX26-KEPT-ORDINARY-THINKING")
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_expired_auxiliary_keeps_the_original_thinking_projection() {
        let source = source();
        let legacy = decode(frame(&source, None), &source);
        let current = decode(frame(&source, Some(1)), &source);
        assert_eq!(legacy.transcript, current.transcript);
        assert_eq!(legacy.content_digest, current.content_digest);
        assert_eq!(current.codex_usage.unwrap(), CodexThreadUsage::default());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_bad_optional_payload_becomes_unknown_without_losing_body() {
        let source = source();
        let original = frame(&source, Some(Utc::now().timestamp_millis() + 10_000));
        let body = decode(original.clone(), &source).transcript;
        for payload in [
            b"{".to_vec(),
            vec![b'x'; CODEX_CHILD_USAGE_CAP + 1],
            br#"{"schema_version":99,"usage_state":"complete","token_usage":null}"#.to_vec(),
            br#"{"schema_version":1,"usage_state":"unknown","token_usage":null,"cost_usd":0.2}"#
                .to_vec(),
        ] {
            let decoded = decode(replace_usage(&original, &payload), &source);
            assert_eq!(decoded.transcript, body);
            assert_eq!(decoded.codex_usage.unwrap(), CodexThreadUsage::default());
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_multiline_redaction_cannot_hide_an_exact_response_as_complete() {
        let mut source = source();
        let mut rows = std::str::from_utf8(&source.bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let mut hidden = rows.last().unwrap().clone();
        hidden["payload"]["response_id"] = serde_json::json!("cx26-hidden-response");
        let message = |text: &str| {
            serde_json::json!({"type":"response_item","payload":{
            "type":"message","role":"assistant","content":[{"type":"output_text","text":text}] }})
        };
        rows.extend([
            message("-----BEGIN PRIVATE KEY-----"),
            hidden,
            message("-----END PRIVATE KEY-----"),
        ]);
        source.bytes = (rows
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .into_bytes();
        let legacy = decode(frame(&source, None), &source);
        let current = decode(
            frame(&source, Some(Utc::now().timestamp_millis() + 10_000)),
            &source,
        );
        let original_records = source.bytes.iter().filter(|byte| **byte == b'\n').count();
        let retained_records = current
            .transcript
            .as_ref()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        assert!(
            retained_records < original_records,
            "must exercise actual record-spanning redaction"
        );
        assert_eq!(legacy.transcript, current.transcript);
        assert_eq!(legacy.content_digest, current.content_digest);
        assert_eq!(legacy.report_value, current.report_value);
        let usage = current.codex_usage.unwrap();
        assert_eq!(usage.usage_state, CodexUsageState::Partial);
        let counters = usage.token_usage.expect("retain known usage subtotal");
        assert_eq!(counters.input_tokens, 3);
        assert_eq!(counters.output_tokens, 2);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_inline_redaction_keeps_complete_numeric_usage() {
        let mut source = source();
        let secret = format!("sk_{}{}", "live_", "A1b2C3d4".repeat(4));
        let message = serde_json::json!({"type":"response_item","payload":{
            "type":"message","role":"assistant","content":[{
                "type":"output_text","text":secret}]}});
        source
            .bytes
            .extend_from_slice(message.to_string().as_bytes());
        source.bytes.push(b'\n');
        let legacy = decode(frame(&source, None), &source);
        let current = decode(
            frame(&source, Some(Utc::now().timestamp_millis() + 10_000)),
            &source,
        );
        let text = std::str::from_utf8(current.transcript.as_ref()).unwrap();
        assert!(!text.contains(&secret));
        assert!(text.contains("<REDACTED:stripe-key>"));
        assert!(text.contains("CX26-KEPT-ORDINARY-THINKING"));
        assert_eq!(legacy.transcript, current.transcript);
        assert_eq!(legacy.content_digest, current.content_digest);
        assert_eq!(legacy.report_value, current.report_value);
        let usage = current.codex_usage.unwrap();
        assert_eq!(usage.usage_state, CodexUsageState::Complete);
        assert_eq!(usage.token_usage.unwrap().input_tokens, 3);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_candidate_custom_numeric_policy_yields_unknown_not_another_value() {
        let usage = CodexThreadUsage {
            schema_version: 1,
            usage_state: CodexUsageState::Complete,
            token_usage: Some(CodexUsageCounters {
                input_tokens: 3,
                output_tokens: 2,
                cached_tokens: Some(1),
                reasoning_tokens: Some(1),
                total_tokens: Some(5),
            }),
        };
        assert_eq!(
            usage
                .clone()
                .after_metadata_redaction(&Redactor::new_default()),
            usage
        );
        let large = CodexThreadUsage {
            schema_version: 1,
            usage_state: CodexUsageState::Complete,
            token_usage: Some(CodexUsageCounters {
                input_tokens: u64::MAX,
                output_tokens: 0,
                cached_tokens: Some(1),
                reasoning_tokens: Some(0),
                total_tokens: Some(u64::MAX),
            }),
        };
        assert_eq!(
            large
                .clone()
                .after_metadata_redaction(&Redactor::new_default()),
            large
        );
        // Current default rules do not scrub bare counters. Exercise a future
        // stricter policy explicitly rather than claiming a default-rule bug.
        let policy = Redactor::with_rules(vec![
            super::super::observed_agents::redaction::RedactionRule {
                id: "test-usage-number",
                regex: regex::bytes::Regex::new(r#""input_tokens"\s*:\s*3"#).unwrap(),
                replacement: "test-usage-number",
            },
        ]);
        assert_eq!(
            usage.after_metadata_redaction(&policy),
            CodexThreadUsage::default()
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_total_frame_corruption_is_still_rejected() {
        let source = source();
        let mut original = frame(&source, Some(Utc::now().timestamp_millis() + 10_000));
        original.pop();
        assert!(decode_subagent_projection_frame(original, source.bytes.len() as u64).is_err());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_full_report_uses_legacy_frame_instead_of_displacing_thinking() {
        let source = source();
        let mut projection = decode(frame(&source, Some(1)), &source);
        projection.report_value["padding"] = serde_json::json!("");
        let binding_len = projection
            .codex_binding
            .as_ref()
            .unwrap()
            .encode()
            .unwrap()
            .len()
            + 2;
        let base = serde_json::to_vec(&projection.report_value).unwrap().len();
        projection.report_value["padding"] = serde_json::json!(
            "x".repeat(SUBAGENT_PROJECTION_REPORT_CAP as usize - binding_len - base)
        );
        let body = projection.transcript.clone();
        let encoded =
            encode_subagent_projection_frame(projection, source.bytes.len() as u64).unwrap();
        assert_eq!(encoded[0], SUBAGENT_PROJECTION_CODEX_COMPLETE);
        let decoded = decode(encoded, &source);
        assert_eq!(decoded.transcript, body);
        assert!(decoded.codex_usage.is_none());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_claude_wire_does_not_gain_a_usage_section() {
        let source = DiscoveredSubagentContent::fixture("claude_code", "fixture/child.jsonl",
            br#"{"type":"assistant","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"kept"}],"usage":{"input_tokens":3,"output_tokens":2}}}
"#, None);
        assert_eq!(
            encode_subagent_projection_request_header(&source).unwrap(),
            encode_subagent_projection_work_request_header(&source, Some(1234)).unwrap()
        );
        let old = frame(&source, None);
        let with_boundary = frame(&source, Some(1234));
        assert_eq!(old, with_boundary);
        assert_eq!(old[0], SUBAGENT_PROJECTION_COMPLETE);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_paired_work_boundary_only_narrows_both_original_clocks() {
        let now = Instant::now();
        let original = CaptureCommitDeadline::from_test_pair(now + Duration::from_secs(2), 1);
        let narrowed = narrow_subagent_work_deadline(
            original,
            original.monotonic() - Duration::from_micros(600),
        )
        .unwrap();
        assert_eq!(
            narrowed.monotonic(),
            original.monotonic() - Duration::from_micros(600)
        );
        assert_eq!(narrowed.sqlite_not_after_millis(), 0);
        assert_eq!(
            narrow_subagent_work_deadline(original, now + Duration::from_secs(10)),
            Some(original)
        );
        for seconds in [1, 8, 30, 60] {
            let original = CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_secs(seconds),
                1,
            );
            let child = import_child_work_deadline(original).unwrap();
            assert!(child.monotonic() < original.monotonic());
            assert!(
                child.monotonic().saturating_duration_since(Instant::now())
                    <= Duration::from_secs(8)
            );
            assert!(child.sqlite_not_after_millis() < original.sqlite_not_after_millis());
            assert!(
                original.monotonic().duration_since(child.monotonic())
                    >= original
                        .monotonic()
                        .saturating_duration_since(Instant::now())
                        / 2
            );
        }
        assert!(
            import_child_work_deadline(CaptureCommitDeadline::from_test_pair(
                Instant::now() - Duration::from_secs(1),
                1,
            ))
            .is_none()
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_metadata_numbers_survive_or_become_unknown_never_another_value() {
        let expected = CodexThreadUsage {
            schema_version: 1,
            usage_state: CodexUsageState::Complete,
            token_usage: Some(CodexUsageCounters {
                input_tokens: u64::MAX,
                output_tokens: 0,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(u64::MAX),
            }),
        };
        let mut usage = Some(expected.clone());
        let mut metadata = serde_json::json!({"subagent":{"usage":expected}});
        let bytes = redacted_subagent_metadata(&mut metadata, &mut usage).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(bytes.as_ref()).unwrap();
        let actual = usage.unwrap();
        assert!(actual == expected || actual == CodexThreadUsage::default());
        assert_eq!(
            decoded["subagent"]["usage"],
            serde_json::to_value(actual).unwrap()
        );
        let mut usage = Some(expected.clone());
        let mut changed = serde_json::json!({"subagent":{"usage":expected}});
        changed["subagent"]["usage"]["token_usage"]["input_tokens"] = serde_json::json!(0);
        let bytes = redacted_subagent_metadata(&mut changed, &mut usage).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(bytes.as_ref()).unwrap();
        assert_eq!(usage, Some(CodexThreadUsage::default()));
        assert!(decoded["subagent"]["usage"]["token_usage"].is_null());
    }
}

#[cfg(all(test, feature = "test-live-agent", unix))]
mod codex_child_usage_enumeration_local_tests {
    use serde_json::{Value, json};

    use super::*;

    const ROOT: &str = "123e4567-e89b-12d3-a456-426614174031";
    const CHILD: &str = "123e4567-e89b-12d3-a456-426614174099";
    const GRAND: &str = "123e4567-e89b-12d3-a456-426614174100";
    const SIBLING: &str = "123e4567-e89b-12d3-a456-426614174098";

    struct Fixture {
        _directory: tempfile::TempDir,
        home: PathBuf,
        cwd: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path().join("home");
            let cwd = directory.path().join("repo");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&cwd).unwrap();
            Self {
                home: home.canonicalize().unwrap(),
                cwd: cwd.canonicalize().unwrap(),
                _directory: directory,
            }
        }
        fn path(&self, id: &str) -> PathBuf {
            self.home
                .join("sessions/2026/10/19")
                .join(format!("rollout-2026-10-19T01-00-00-{id}.jsonl"))
        }
        fn write(&self, id: &str, parent: Option<&str>, spawned: &[&str]) {
            let mut header =
                json!({"id":id,"session_id":ROOT,"cwd":self.cwd,"cli_version":"0.162.1"});
            if let Some(parent) = parent {
                header["parent_thread_id"] = parent.into();
            }
            let mut rows = vec![
                json!({"type":"session_meta","payload":header}),
                json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}),
                json!({"type":"turn_context","payload":{"turn_id":"t","model":"fixture-model"}}),
                json!({"type":"response_item","payload":{"type":"reasoning","id":format!("reason-{id}"),"summary":[{"type":"summary_text","text":format!("THINKING-{id}")}]}}),
            ];
            for child in spawned {
                rows.push(json!({"type":"event_msg","payload":{"type":"collab_agent_spawn_end","sender_thread_id":id,"new_thread_id":child}}));
            }
            let path = self.path(id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            )
            .unwrap();
        }
        fn query(&self, current: &str) -> CodexChildDiscoveryQuery {
            CodexChildDiscoveryQuery {
                capture_session: "parent-session".into(),
                selection: CodexChildDiscoverySelection::Import {
                    current: current.into(),
                    current_path: Some(self.path(current).to_str().unwrap().into()),
                },
            }
        }
        fn discover(&self, current: &str, limit: usize) -> SubagentDiscovery {
            discover_codex_subagent_contents_in_roots(
                &self.cwd,
                &self.query(current),
                CodexDiscoveryBudget {
                    spent: 0,
                    cap: 16 * 1024 * 1024,
                    deadline: Instant::now() + Duration::from_secs(5),
                },
                limit,
                vec![CodexPinnedDiscoveryRoot {
                    path: self.home.clone(),
                    directory: std::fs::File::open(&self.home).unwrap(),
                }],
                false,
            )
            .unwrap()
        }
        fn tree(&self) {
            self.write(ROOT, None, &[CHILD, SIBLING]);
            self.write(CHILD, Some(ROOT), &[GRAND]);
            self.write(GRAND, Some(CHILD), &[]);
            self.write(SIBLING, Some(ROOT), &[]);
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_enumeration_missing_body_evidence_keeps_selected_descendant() {
        let fixture = Fixture::new();
        fixture.tree();
        const GREAT: &str = "123e4567-e89b-12d3-a456-426614174105";
        fixture.write(GREAT, Some(GRAND), &[]);
        let roots = vec![CodexPinnedDiscoveryRoot {
            path: fixture.home.clone(),
            directory: std::fs::File::open(&fixture.home).unwrap(),
        }];
        let mut budget = CodexDiscoveryBudget {
            spent: 0,
            cap: 16 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let candidate = CodexDiscoveryCandidate {
            root: 0,
            relative: fixture
                .path(GREAT)
                .strip_prefix(&fixture.home)
                .unwrap()
                .to_path_buf(),
        };
        let query = fixture.query(ROOT);
        let (_, _, header) = codex_candidate_header(
            &roots,
            &candidate,
            &fixture.cwd,
            &mut budget,
            CodexHeaderMode::ChildScan,
        )
        .unwrap();
        // Use a nested current thread distinct from both this node's parent
        // and the global root: membership was already checked by the caller.
        let mut query = query;
        query.selection = CodexChildDiscoverySelection::Import {
            current: CHILD.into(),
            current_path: None,
        };
        let source = read_codex_discovered_child_with_header(
            &roots,
            &candidate,
            &fixture.cwd,
            &query,
            None,
            &mut budget,
            Some(&header),
        )
        .unwrap()
        .unwrap();
        assert!(source.partial);
        assert!(!source.bytes.is_empty());
        assert_eq!(source.codex_binding.unwrap().child, GREAT);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_enumeration_header_order_and_nested_subtree_are_complete() {
        let fixture = Fixture::new();
        fixture.tree();
        let all = fixture.discover(ROOT, 16);
        assert_eq!(all.sources.len(), 3, "{all:?}");
        assert!(!all.incomplete, "{all:?}");
        assert!(
            all.usage_enumeration.as_ref().unwrap().exhaustive,
            "{all:?}"
        );
        assert!(
            all.bytes_read
                >= all
                    .sources
                    .iter()
                    .map(|source| source.bytes.len() as u64)
                    .sum::<u64>()
        );
        let nested = fixture.discover(CHILD, 16);
        assert_eq!(nested.sources.len(), 1, "{nested:?}");
        assert!(!nested.incomplete, "{nested:?}");
        assert!(
            nested.usage_enumeration.as_ref().unwrap().exhaustive,
            "{nested:?}"
        );
        let binding = nested.sources[0].codex_binding.as_ref().unwrap();
        assert_eq!(binding.parent, CHILD);
        assert_eq!(binding.child, GRAND);
        assert!(binding.hook_turn.is_none());
        for source in all.sources {
            let projection =
                safe_codex_content_projection_with_usage_owned(source, 16 * 1024 * 1024, None)
                    .unwrap();
            assert!(
                !projection.partial,
                "projection with acquired thinking must stay complete"
            );
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_enumeration_missing_ancestor_preserves_body_without_exhaustive_claim() {
        let fixture = Fixture::new();
        fixture.tree();
        std::fs::remove_file(fixture.path(CHILD)).unwrap();
        let result = fixture.discover(ROOT, 16);
        assert!(result.incomplete);
        assert!(!result.usage_enumeration.as_ref().unwrap().exhaustive);
        assert!(result.sources.iter().any(|source| {
            source
                .bytes
                .windows(b"THINKING-".len())
                .any(|window| window == b"THINKING-")
        }));
        assert!(result.sources.iter().any(|source|source.stable_subagent_id.as_deref()==Some(GRAND) && source.partial));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_enumeration_source_limit_preserves_acquired_thinking() {
        let fixture = Fixture::new();
        fixture.tree();
        let result = fixture.discover(ROOT, 1);
        assert_eq!(result.sources.len(), 1);
        assert!(result.incomplete);
        assert!(!result.usage_enumeration.as_ref().unwrap().exhaustive);
        assert!(String::from_utf8_lossy(&result.sources[0].bytes).contains("THINKING-"));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_usage_enumeration_optional_codec_is_bound_and_malformed_proof_keeps_data() {
        let fixture = Fixture::new();
        fixture.tree();
        let query = fixture.query(ROOT);
        let discovery = fixture.discover(ROOT, 16);
        let original =
            encode_subagent_discovery_helper_success_with_query(discovery.clone(), Some(&query))
                .unwrap();
        let header_len = u32::from_le_bytes(original[..4].try_into().unwrap()) as usize;
        let header: Value = serde_json::from_slice(&original[4..4 + header_len]).unwrap();
        assert_eq!(
            header["usage_enumeration"],
            json!({"selection":"import","exhaustive":true})
        );
        let decode = |frame| {
            decode_subagent_discovery_helper_frame_with_query(
                frame,
                16 * 1024 * 1024,
                16,
                Instant::now() + Duration::from_secs(3),
                Some((&query, &fixture.cwd)),
            )
            .unwrap()
        };
        let decoded = decode(original.clone());
        assert!(
            decoded
                .usage_enumeration
                .as_ref()
                .unwrap()
                .matches_query(&query)
        );
        for malformed in [
            json!({"selection":"boundary","exhaustive":true}),
            json!({"selection":"import","exhaustive":"bad"}),
            json!({"selection":"import","exhaustive":true,"extra":1}),
            Value::Null,
        ] {
            let mut changed = header.clone();
            changed["usage_enumeration"] = malformed;
            let bytes = serde_json::to_vec(&changed).unwrap();
            let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
            frame.extend(bytes);
            frame.extend_from_slice(&original[4 + header_len..]);
            let decoded = decode(frame);
            assert!(decoded.usage_enumeration.is_none());
            assert_eq!(decoded.sources.len(), discovery.sources.len());
            for (old, new) in discovery.sources.iter().zip(&decoded.sources) {
                assert_eq!(old.bytes, new.bytes);
            }
        }
    }
}

#[cfg(test)]
mod storage_config_upcast_subagent_tests {
    use super::ClientStorage;

    #[tokio::test]
    #[serial_test::serial(env)]
    async fn storage_config_upcast_subagent_rejects_confirmed_type() {
        let _type = crate::utils::test::ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "gcs-canary");
        let dir = tempfile::tempdir().expect("temp");
        let error = match ClientStorage::open_init_storage(dir.path().join("objects")) {
            Ok(_) => panic!("confirmed type must not open storage"),
            Err(error) => error,
        };
        let text = format!(
            "failed to read storage configuration for {}",
            error.variable()
        );
        assert_eq!(
            text,
            "failed to read storage configuration for LIBRA_STORAGE_TYPE"
        );
        assert!(!text.contains("gcs-canary"));
    }

    #[tokio::test]
    #[serial_test::serial(env)]
    async fn storage_config_upcast_subagent_local_word_is_ready() {
        let _type = crate::utils::test::ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let dir = tempfile::tempdir().expect("temp");
        let storage = match ClientStorage::open_init_storage(dir.path().join("objects")) {
            Ok(storage) => storage,
            Err(error) => panic!("local word must stay ready: {error}"),
        };
        assert!(storage.configuration_variable().is_none());
    }
}
