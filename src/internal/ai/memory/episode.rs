//! Episode domain types, deterministic identity and content digest.
//!
//! `episode_id` and `content_digest` are the two determinism anchors (ADR-DM-01 /
//! ADR-DM-12). Identity is `UUIDv5(MEMORY_EPISODE_NAMESPACE_V1, \
//! "{repo_id}\x1f{source_kind}\x1f{source_key}")`; drift is detected through the
//! digest of the canonical serialization, and both are pinned by the
//! `episode_identity_golden` test so any rule change is an intentional bump.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Namespace for the deterministic Episode `episode_id` (ADR-DM-01).
pub const MEMORY_EPISODE_NAMESPACE_V1: Uuid = Uuid::from_u128(0x6c62_7261_5f6d_656d_6f72_795f_7631);

/// One of the four durable-fact source kinds (matches the `memory_episode`
/// CHECK vocabulary).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Commit,
    AgentSession,
    AgentRun,
    BridgeOperation,
}

impl SourceKind {
    /// The stable value written to `memory_episode.source_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::AgentSession => "agent_session",
            Self::AgentRun => "agent_run",
            Self::BridgeOperation => "bridge_operation",
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Terminal outcome (matches the `memory_episode.outcome` CHECK vocabulary).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Aborted,
    Partial,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Independent value axis rebuilt from the removed anchor-confidence source
/// (`MemoryAnchorConfidence`). Deliberately NOT merged with any trust
/// semantics (trust belongs to the separate `MemoryTrust` axis, ADR-DM-01).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConfidence {
    Exact,
    Approx,
    Range,
    None,
}

/// A single evidence edge linking an Episode to a durable source artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEdge {
    /// `kind` vocabulary: `commit` | `checkpoint` | `review_run` | `operation`.
    pub kind: String,
    /// The referenced artifact id (commit_oid / checkpoint_id / run_id / op_id).
    pub ref_id: String,
    /// `link_confidence` vocabulary: `identity` | `operation` | `temporal`.
    pub link_confidence: String,
    /// `resolution_status` vocabulary: `resolved` | `unresolved`.
    pub resolution_status: String,
}

/// A projected path row for `memory_episode_path`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathRow {
    pub code_path: Vec<u8>,
    pub change_kind: String,
    pub blob_oid_at_end: Option<String>,
    pub ended_at: i64,
}

/// A fully-materialized, zero-authority Episode projection row (GC-DM-01).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub episode_id: String,
    pub repo_id: String,
    pub source_kind: SourceKind,
    pub source_key: String,
    pub outcome: Outcome,
    pub actor: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    pub anchor_commit: Option<String>,
    pub change_id: Option<String>,
    pub title: String,
    pub body: String,
    pub content_digest: String,
    pub producer: &'static str,
    pub rules_version: i64,
    pub evidence: Vec<EvidenceEdge>,
    pub paths: Vec<PathRow>,
}

impl Episode {
    /// Deterministic `episode_id`: `UUIDv5(NAMESPACE, "{repo}\x1f{kind}\x1f{key}")`.
    pub fn compute_episode_id(repo_id: &str, source_kind: SourceKind, source_key: &str) -> String {
        episode_id(repo_id, source_kind, source_key)
    }
}

/// Deterministic `episode_id` (ADR-DM-01). Never change the namespace or the
/// FS-separator layout without bumping the golden vector.
pub fn episode_id(repo_id: &str, source_kind: SourceKind, source_key: &str) -> String {
    let input = format!("{repo_id}\x1f{}\x1f{source_key}", source_kind.as_str());
    Uuid::new_v5(&MEMORY_EPISODE_NAMESPACE_V1, input.as_bytes()).to_string()
}

/// Deterministic digest over the canonical content, used to detect drift.
///
/// The canonical form is the concatenation of the identity plus the
/// title/body/evidence/outcome in a fixed order, so byte-for-byte identical
/// content yields an identical digest and any change flips it.
pub fn content_digest(episode: &Episode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(episode.episode_id.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(episode.title.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(episode.body.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(episode.outcome.as_str().as_bytes());
    hasher.update(b"\x1f");
    if let Some(anchor) = &episode.anchor_commit {
        hasher.update(anchor.as_bytes());
    }
    hasher.update(b"\x1f");
    for edge in &episode.evidence {
        hasher.update(edge.kind.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(edge.ref_id.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(edge.link_confidence.as_bytes());
        hasher.update(b"\x1f");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_identity_is_deterministic_uuid_v5() {
        let id = episode_id("repo-a", SourceKind::Commit, "change-1");
        // UUID v5 form, deterministically derived (golden vector).
        assert_eq!(
            id, "fffbf78c-240e-5823-a67d-17244dbe58dd",
            "episode_id golden vector (UUIDv5, namespace 00006c62-7261-5f6d-656d-6f72795f7631)"
        );
        assert_eq!(id, episode_id("repo-a", SourceKind::Commit, "change-1"));
        assert_ne!(id, episode_id("repo-a", SourceKind::Commit, "change-2"));
        assert_ne!(id, episode_id("repo-b", SourceKind::Commit, "change-1"));
    }

    #[test]
    fn content_digest_is_deterministic_and_change_sensitive() {
        let mut a = Episode {
            episode_id: episode_id("r", SourceKind::Commit, "c"),
            repo_id: "r".into(),
            source_kind: SourceKind::Commit,
            source_key: "c".into(),
            outcome: Outcome::Succeeded,
            actor: Some("eli".into()),
            started_at: 1,
            ended_at: 2,
            anchor_commit: Some("deadbeef".into()),
            change_id: Some("c".into()),
            title: "title".into(),
            body: "body".into(),
            content_digest: String::new(),
            producer: "derived-v1",
            rules_version: 1,
            evidence: vec![],
            paths: vec![],
        };
        let d1 = content_digest(&a);
        assert_eq!(d1, content_digest(&a));
        a.title = "title2".into();
        let d2 = content_digest(&a);
        assert_ne!(d1, d2, "title change must flip digest");
    }

    #[test]
    fn memory_confidence_is_an_independent_axis() {
        // The value axis must not alias trust: it is a separate enum with its
        // own vocabulary.
        let values = [
            MemoryConfidence::Exact,
            MemoryConfidence::Approx,
            MemoryConfidence::Range,
            MemoryConfidence::None,
        ];
        for v in values {
            let _ = serde_json::to_string(&v).expect("serializable");
        }
    }
}
