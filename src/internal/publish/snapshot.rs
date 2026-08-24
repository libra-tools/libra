//! Publish snapshot builder — Phase 3 of `docs/development/commands/publish.md`.
//!
//! The snapshot builder takes a list of refs (`refs/heads/*` and
//! `refs/tags/*`), resolves each to a commit oid, dedupes by
//! revision oid, and produces, for every unique revision:
//!
//!   * a `code-manifest.json` listing every published path with
//!     `display_mode` (text / binary / too_large / ignored),
//!     content sha256, R2 key, size, language;
//!   * one R2 blob per text file, keyed by sha256 (so two revisions
//!     pointing at the same text content share a single blob);
//!   * a `RevisionPlan` the orchestrator uses to populate D1.
//!
//! The AI object model exporter (`ai_export.rs`) is a separate
//! sibling module so the code path stays self-contained — Phase 3
//! lands the code snapshot first and AI export second to keep the
//! review iteration tight.
//!
//! No D1/R2 I/O happens in this module — the builder produces
//! plans and (optionally) writes blobs through the
//! `publish_storage::PublishStorage` wrapper passed by the
//! orchestrator. The CLI sync command (Phase 4) drives the whole
//! pipeline.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

use crate::internal::publish::{
    contract::{
        FileDisplayMode, PUBLISH_SCHEMA_VERSION, PublishCodeManifest, PublishFile, PublishRefEntry,
        PublishRefsIndex, RefType,
    },
    preflight::{Preflight, PreflightDecision},
};

/// Maximum file size that gets a R2 preview blob, in bytes.
///
/// Files larger than `max_preview_bytes` are recorded as
/// `display_mode = too_large` with metadata only. The default
/// (1 MiB) matches publish.md `--max-preview-bytes` documentation;
/// callers override via `SnapshotConfig::max_preview_bytes`.
pub const DEFAULT_MAX_PREVIEW_BYTES: u64 = 1024 * 1024;

/// Caller-supplied snapshot configuration.
#[derive(Clone, Debug)]
pub struct SnapshotConfig {
    pub max_preview_bytes: u64,
    pub preflight: Preflight,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            max_preview_bytes: DEFAULT_MAX_PREVIEW_BYTES,
            preflight: Preflight::default(),
        }
    }
}

/// One ref the orchestrator wants published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefInput {
    /// Full ref name, `refs/heads/<name>` or `refs/tags/<name>`.
    pub ref_name: String,
    /// What the ref points at directly. For lightweight tags this
    /// is the commit oid; for annotated tags this is the tag oid
    /// (the orchestrator passes the unresolved target so the
    /// builder can record the annotated tag in D1 with its own oid).
    pub target_oid: String,
    /// The peeled commit oid the snapshot was built from. For
    /// lightweight tags + branches, equal to `target_oid`. For
    /// annotated tags, the tag's `object` field.
    pub revision_oid: String,
}

/// Result of evaluating one file inside a tree walk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileSnapshot {
    /// Text file with R2 content.
    Text {
        path: String,
        size_bytes: u64,
        content_sha256: String,
        language: Option<String>,
    },
    /// Binary file: metadata only, no R2 blob.
    Binary { path: String, size_bytes: u64 },
    /// File larger than `max_preview_bytes`: metadata only.
    TooLarge { path: String, size_bytes: u64 },
    /// File matched a deny rule or `.librapublishignore`: metadata
    /// only, with a tag identifying which rule fired.
    Ignored {
        path: String,
        size_bytes: u64,
        reason: IgnoredReason,
    },
}

/// Why a file landed in `Ignored`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoredReason {
    BuiltinCredential,
    UserIgnore,
}

/// Plan for one unique revision. The orchestrator turns this into
/// `publish_revisions` + `publish_files` D1 rows and `code-manifest.json`
/// in R2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevisionPlan {
    pub revision_oid: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub files: Vec<FileSnapshot>,
    pub generated_at: DateTime<Utc>,
}

/// File body supplied by the sync orchestrator before classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionFileInput {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// One text blob that must be materialised in R2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextBlobUpload {
    pub path: String,
    pub content_sha256: String,
    pub relative_key: String,
    pub object_key: String,
    pub bytes: Vec<u8>,
}

/// Complete code snapshot artefacts for one revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionArtifactPlan {
    pub revision: RevisionPlan,
    pub code_manifest_relative_key: String,
    pub code_manifest_key: String,
    pub code_manifest: PublishCodeManifest,
    pub text_blobs: Vec<TextBlobUpload>,
}

impl RevisionPlan {
    /// Aggregate file count across every kind.
    pub fn total_file_count(&self) -> usize {
        self.files.len()
    }

    /// Count how many files were materialised to R2 (text only).
    pub fn r2_blob_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f, FileSnapshot::Text { .. }))
            .count()
    }

    /// Convert this revision snapshot into the JSON manifest shape
    /// written at
    /// `{repo_id}/publish/sites/{site_id}/revisions/{revision_oid}/code-manifest.json`.
    pub fn to_code_manifest(&self, repo_id: &str, site_id: &str) -> PublishCodeManifest {
        PublishCodeManifest {
            schema_version: PUBLISH_SCHEMA_VERSION,
            site_id: site_id.to_string(),
            revision_oid: self.revision_oid.clone(),
            commit_oid: self.commit_oid.clone(),
            tree_oid: self.tree_oid.clone(),
            generated_at: self.generated_at,
            files: self
                .files
                .iter()
                .map(|file| file.to_publish_file(repo_id, site_id, &self.revision_oid))
                .collect(),
        }
    }
}

/// Build the code snapshot artefacts the sync path persists to D1/R2.
///
/// Text files become R2 blob uploads and manifest/D1 rows. Binary,
/// too-large and ignored paths only appear in the manifest/D1 metadata.
#[allow(clippy::too_many_arguments)]
pub fn build_revision_artifact_plan(
    repo_id: &str,
    site_id: &str,
    revision_oid: &str,
    commit_oid: &str,
    tree_oid: &str,
    generated_at: DateTime<Utc>,
    files: Vec<RevisionFileInput>,
    config: &SnapshotConfig,
) -> Result<RevisionArtifactPlan, SnapshotBuildError> {
    validate_oid(revision_oid)?;
    validate_oid(commit_oid)?;
    validate_oid(tree_oid)?;

    let mut snapshots = Vec::with_capacity(files.len());
    let mut text_blobs = Vec::new();
    for file in files {
        let snapshot = classify_file(&file.path, &file.bytes, config)?;
        if let FileSnapshot::Text { content_sha256, .. } = &snapshot {
            text_blobs.push(TextBlobUpload {
                path: file.path.clone(),
                content_sha256: content_sha256.clone(),
                relative_key: publish_text_file_relative_key(revision_oid, content_sha256),
                object_key: publish_text_file_key(repo_id, site_id, revision_oid, content_sha256),
                bytes: file.bytes,
            });
        }
        snapshots.push(snapshot);
    }

    let revision = RevisionPlan {
        revision_oid: revision_oid.to_string(),
        commit_oid: commit_oid.to_string(),
        tree_oid: tree_oid.to_string(),
        files: snapshots,
        generated_at,
    };
    let code_manifest = revision.to_code_manifest(repo_id, site_id);
    Ok(RevisionArtifactPlan {
        revision,
        code_manifest_relative_key: publish_code_manifest_relative_key(revision_oid),
        code_manifest_key: publish_code_manifest_key(repo_id, site_id, revision_oid),
        code_manifest,
        text_blobs,
    })
}

impl FileSnapshot {
    /// Convert this file snapshot into the `publish_files`/manifest
    /// contract row. Only text files get a content hash and R2 key;
    /// binary, too-large, and ignored files stay metadata-only.
    pub fn to_publish_file(&self, repo_id: &str, site_id: &str, revision_oid: &str) -> PublishFile {
        match self {
            Self::Text {
                path,
                size_bytes,
                content_sha256,
                language,
            } => PublishFile {
                site_id: site_id.to_string(),
                revision_oid: revision_oid.to_string(),
                path: path.clone(),
                display_mode: FileDisplayMode::Text,
                content_sha256: Some(content_sha256.clone()),
                r2_key: Some(publish_text_file_key(
                    repo_id,
                    site_id,
                    revision_oid,
                    content_sha256,
                )),
                size_bytes: *size_bytes,
                language: language.clone(),
            },
            Self::Binary { path, size_bytes } => PublishFile {
                site_id: site_id.to_string(),
                revision_oid: revision_oid.to_string(),
                path: path.clone(),
                display_mode: FileDisplayMode::Binary,
                content_sha256: None,
                r2_key: None,
                size_bytes: *size_bytes,
                language: None,
            },
            Self::TooLarge { path, size_bytes } => PublishFile {
                site_id: site_id.to_string(),
                revision_oid: revision_oid.to_string(),
                path: path.clone(),
                display_mode: FileDisplayMode::TooLarge,
                content_sha256: None,
                r2_key: None,
                size_bytes: *size_bytes,
                language: None,
            },
            Self::Ignored {
                path, size_bytes, ..
            } => PublishFile {
                site_id: site_id.to_string(),
                revision_oid: revision_oid.to_string(),
                path: path.clone(),
                display_mode: FileDisplayMode::Ignored,
                content_sha256: None,
                r2_key: None,
                size_bytes: *size_bytes,
                language: None,
            },
        }
    }
}

pub fn publish_code_manifest_relative_key(revision_oid: &str) -> String {
    format!("revisions/{revision_oid}/code-manifest.json")
}

pub fn publish_code_manifest_key(repo_id: &str, site_id: &str, revision_oid: &str) -> String {
    format!(
        "{repo_id}/publish/sites/{site_id}/{}",
        publish_code_manifest_relative_key(revision_oid)
    )
}

pub fn publish_text_file_relative_key(revision_oid: &str, content_sha256: &str) -> String {
    format!("revisions/{revision_oid}/files/{content_sha256}.txt")
}

pub fn publish_text_file_key(
    repo_id: &str,
    site_id: &str,
    revision_oid: &str,
    content_sha256: &str,
) -> String {
    format!(
        "{repo_id}/publish/sites/{site_id}/{}",
        publish_text_file_relative_key(revision_oid, content_sha256)
    )
}

pub fn publish_refs_index_key(repo_id: &str, site_id: &str) -> String {
    format!("{repo_id}/publish/sites/{site_id}/refs.json")
}

/// Build the publish refs/revision plan that the sync orchestrator
/// persists to D1.
///
/// Every supplied ref is preserved as a `publish_refs` candidate, while
/// `revisions` is reduced to the unique set referenced by those refs.
/// If two refs point at the same `revision_oid`, they share one
/// `RevisionPlan` and therefore one R2/D1 revision snapshot.
pub fn build_snapshot_plan(
    refs: &[RefInput],
    revisions: Vec<RevisionPlan>,
    default_ref: Option<&str>,
) -> Result<SnapshotPlan, SnapshotBuildError> {
    use std::collections::{BTreeMap, BTreeSet};

    if let Some(default_ref) = default_ref {
        validate_ref_name(default_ref)?;
        if !refs
            .iter()
            .any(|publish_ref| publish_ref.ref_name == default_ref)
        {
            return Err(SnapshotBuildError::InvalidRef {
                message: format!("default ref {default_ref:?} is not included in publish refs"),
            });
        }
    }

    let mut revision_by_oid = BTreeMap::new();
    for revision in revisions {
        validate_oid(&revision.revision_oid)?;
        validate_oid(&revision.commit_oid)?;
        validate_oid(&revision.tree_oid)?;
        revision_by_oid
            .entry(revision.revision_oid.clone())
            .or_insert(revision);
    }

    let mut ref_plans = Vec::with_capacity(refs.len());
    let mut used_revisions = BTreeSet::new();
    let mut unique_revisions = Vec::new();
    for publish_ref in refs {
        validate_ref_name(&publish_ref.ref_name)?;
        validate_oid(&publish_ref.target_oid)?;
        validate_oid(&publish_ref.revision_oid)?;
        let Some(revision) = revision_by_oid.get(&publish_ref.revision_oid) else {
            return Err(SnapshotBuildError::MissingRevision {
                revision_oid: publish_ref.revision_oid.clone(),
            });
        };
        if used_revisions.insert(publish_ref.revision_oid.clone()) {
            unique_revisions.push(revision.clone());
        }
        ref_plans.push(RefPlan {
            ref_name: publish_ref.ref_name.clone(),
            target_oid: publish_ref.target_oid.clone(),
            revision_oid: publish_ref.revision_oid.clone(),
            is_default: default_ref.is_some_and(|name| name == publish_ref.ref_name),
        });
    }

    Ok(SnapshotPlan {
        revisions: unique_revisions,
        refs: ref_plans,
        default_ref: default_ref.map(ToString::to_string),
    })
}

/// Plan for one ref entry. Mirrors the `publish_refs` row shape +
/// the `refs.json` index payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefPlan {
    pub ref_name: String,
    pub target_oid: String,
    pub revision_oid: String,
    pub is_default: bool,
}

impl RefPlan {
    pub fn ref_type(&self) -> RefType {
        if self.ref_name.starts_with("refs/tags/") {
            RefType::Tag
        } else {
            RefType::Branch
        }
    }

    pub fn short_name(&self) -> &str {
        short_ref_name(&self.ref_name).unwrap_or(self.ref_name.as_str())
    }

    pub fn to_publish_ref_entry(&self, updated_at: DateTime<Utc>) -> PublishRefEntry {
        PublishRefEntry {
            ref_name: self.ref_name.clone(),
            ref_type: self.ref_type(),
            short_name: self.short_name().to_string(),
            target_oid: self.target_oid.clone(),
            revision_oid: self.revision_oid.clone(),
            is_default: self.is_default,
            updated_at,
        }
    }
}

/// Full plan returned by [`build_snapshot_plan`]. The orchestrator
/// uses this to populate D1 in the right write order (revisions →
/// files → ai_objects → ai_versions → refs → site latest).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPlan {
    /// Unique revisions, dedupe by `revision_oid`.
    pub revisions: Vec<RevisionPlan>,
    pub refs: Vec<RefPlan>,
    pub default_ref: Option<String>,
}

impl SnapshotPlan {
    /// Return the revision that may update `publish_sites.latest_revision_oid`.
    ///
    /// The latest pointer is intentionally derived only from the default
    /// ref. Other branch/tag refs can be published in the same sync, but
    /// must not move the site-level latest pointer.
    pub fn default_latest_revision_oid(&self) -> Option<&str> {
        let default_ref = self.default_ref.as_deref()?;
        self.refs
            .iter()
            .find(|publish_ref| publish_ref.ref_name == default_ref)
            .map(|publish_ref| publish_ref.revision_oid.as_str())
    }

    pub fn to_refs_index(
        &self,
        site_id: &str,
        refs_generation: u64,
        generated_at: DateTime<Utc>,
    ) -> Result<PublishRefsIndex, SnapshotBuildError> {
        let default_ref =
            self.default_ref
                .clone()
                .ok_or_else(|| SnapshotBuildError::InvalidRef {
                    message: "refs index requires a default ref".to_string(),
                })?;
        if !self
            .refs
            .iter()
            .any(|publish_ref| publish_ref.ref_name == default_ref && publish_ref.is_default)
        {
            return Err(SnapshotBuildError::InvalidRef {
                message: format!("default ref {default_ref:?} is not marked in publish refs"),
            });
        }
        Ok(PublishRefsIndex {
            schema_version: PUBLISH_SCHEMA_VERSION,
            site_id: site_id.to_string(),
            refs_generation,
            default_ref,
            refs: self
                .refs
                .iter()
                .map(|publish_ref| publish_ref.to_publish_ref_entry(generated_at))
                .collect(),
            generated_at,
        })
    }
}

/// Errors surfaced by the snapshot builder.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotBuildError {
    #[error("snapshot ref input is invalid: {message}")]
    InvalidRef { message: String },
    #[error("snapshot revision oid is invalid: {oid}")]
    InvalidOid { oid: String },
    #[error("snapshot revision {revision_oid} is missing from the revision plan")]
    MissingRevision { revision_oid: String },
    #[error("snapshot path {path:?} is not valid UTF-8")]
    NonUtf8Path { path: PathBuf },
    #[error("snapshot file {path:?} could not be hashed")]
    HashFailure { path: PathBuf },
    #[error("snapshot file {path:?} is referenced by an unsupported tree mode")]
    UnsupportedMode { path: PathBuf },
}

/// Hash a file body with sha256 and return the lowercase hex
/// digest. Used both for content-addressed R2 keys and the
/// `content_sha256` column.
pub fn sha256_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Detect whether a byte buffer is "text" (UTF-8, no NUL bytes
/// past the first 8KiB sniff window).
///
/// Mirrors what the Rust ecosystem typically does for diff /
/// preview tooling: a buffer is text iff the first 8KiB is valid
/// UTF-8 with no NUL bytes. `infer` would be more thorough but
/// pulls in a magic-number table; the publish path doesn't need
/// MIME detection beyond the binary/text split.
pub fn is_probably_text(bytes: &[u8]) -> bool {
    let sniff = &bytes[..bytes.len().min(8 * 1024)];
    if sniff.contains(&0) {
        return false;
    }
    std::str::from_utf8(sniff).is_ok()
}

/// Heuristic file-extension → language tag. Used only for the
/// `language` field in `code-manifest.json`; the Worker file viewer
/// uses it for syntax highlighting hints. Not exhaustive; missing
/// extensions surface as `None`.
pub fn language_for_path(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let ext = std::path::Path::new(&lower)
        .extension()
        .and_then(|s| s.to_str())?;
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cxx" | "cc" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "css" => "css",
        "scss" | "sass" => "scss",
        "html" | "htm" => "html",
        "xml" => "xml",
        "lock" => "lockfile",
        "txt" => "text",
        _ => return None,
    })
}

/// Classify one file blob given its repo-relative path, byte body,
/// and the snapshot config. Used by the orchestrator's tree-walk
/// loop. The blob bytes are not retained — callers stream the
/// content to R2 via `PublishStorage::put_bytes` when this
/// returns `FileSnapshot::Text`.
pub fn classify_file(
    path: &str,
    bytes: &[u8],
    config: &SnapshotConfig,
) -> Result<FileSnapshot, SnapshotBuildError> {
    let path_buf = PathBuf::from(path);
    match config.preflight.evaluate(&path_buf, false) {
        PreflightDecision::Allow => {}
        PreflightDecision::Deny(reason) => {
            return Ok(FileSnapshot::Ignored {
                path: path.to_string(),
                size_bytes: bytes.len() as u64,
                reason: match reason {
                    crate::internal::publish::preflight::DenyReason::BuiltinCredential => {
                        IgnoredReason::BuiltinCredential
                    }
                    crate::internal::publish::preflight::DenyReason::UserIgnore => {
                        IgnoredReason::UserIgnore
                    }
                },
            });
        }
    }
    let size_bytes = bytes.len() as u64;
    if size_bytes > config.max_preview_bytes {
        return Ok(FileSnapshot::TooLarge {
            path: path.to_string(),
            size_bytes,
        });
    }
    if !is_probably_text(bytes) {
        return Ok(FileSnapshot::Binary {
            path: path.to_string(),
            size_bytes,
        });
    }
    let content_sha256 = sha256_hex(bytes);
    Ok(FileSnapshot::Text {
        path: path.to_string(),
        size_bytes,
        content_sha256,
        language: language_for_path(path).map(|s| s.to_string()),
    })
}

/// Validate a hex object id (4..=64 lowercase hex chars).
pub fn validate_oid(value: &str) -> Result<(), SnapshotBuildError> {
    if value.len() < 4 || value.len() > 64 {
        return Err(SnapshotBuildError::InvalidOid {
            oid: value.to_string(),
        });
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase()))
    {
        return Err(SnapshotBuildError::InvalidOid {
            oid: value.to_string(),
        });
    }
    Ok(())
}

/// Validate a ref name: must be `refs/heads/<short>` or
/// `refs/tags/<short>` per publish.md acceptance criteria.
pub fn validate_ref_name(value: &str) -> Result<(), SnapshotBuildError> {
    if value.starts_with("refs/heads/") {
        let short = value.trim_start_matches("refs/heads/");
        if short.is_empty() {
            return Err(SnapshotBuildError::InvalidRef {
                message: "refs/heads/ ref must carry a short name".to_string(),
            });
        }
        Ok(())
    } else if value.starts_with("refs/tags/") {
        let short = value.trim_start_matches("refs/tags/");
        if short.is_empty() {
            return Err(SnapshotBuildError::InvalidRef {
                message: "refs/tags/ ref must carry a short name".to_string(),
            });
        }
        Ok(())
    } else {
        Err(SnapshotBuildError::InvalidRef {
            message: format!("ref name {value:?} must start with refs/heads/ or refs/tags/"),
        })
    }
}

pub fn short_ref_name(full_ref: &str) -> Option<&str> {
    full_ref
        .strip_prefix("refs/heads/")
        .or_else(|| full_ref.strip_prefix("refs/tags/"))
}

/// Detect ambiguous short refs: a branch and a tag with the same
/// short name. The CLI rejects ambiguous `--ref <short>` requests
/// per publish.md; this helper produces the shape the caller can
/// surface as a typed error.
pub fn detect_ambiguous_short_refs(refs: &[RefInput]) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut by_short: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for r in refs {
        let (kind, short) = if let Some(s) = r.ref_name.strip_prefix("refs/heads/") {
            ("branch", s)
        } else if let Some(s) = r.ref_name.strip_prefix("refs/tags/") {
            ("tag", s)
        } else {
            continue;
        };
        by_short.entry(short.to_string()).or_default().push(kind);
    }
    by_short
        .into_iter()
        .filter_map(|(short, kinds)| {
            let has_branch = kinds.contains(&"branch");
            let has_tag = kinds.contains(&"tag");
            if has_branch && has_tag {
                Some(short)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SnapshotConfig {
        SnapshotConfig::default()
    }

    #[test]
    fn classify_text_is_text() {
        let snapshot = classify_file("README.md", b"# hello\n", &cfg()).unwrap();
        match snapshot {
            FileSnapshot::Text {
                path,
                size_bytes,
                content_sha256,
                language,
            } => {
                assert_eq!(path, "README.md");
                assert_eq!(size_bytes, 8);
                assert_eq!(content_sha256.len(), 64);
                assert_eq!(language.as_deref(), Some("markdown"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn classify_binary_when_nul_byte_present() {
        let bytes = b"\x00\x01\x02\x03binary".to_vec();
        let snapshot = classify_file("logo.png", &bytes, &cfg()).unwrap();
        assert!(matches!(snapshot, FileSnapshot::Binary { .. }));
    }

    #[test]
    fn classify_too_large_uses_max_preview_bytes() {
        let mut config = cfg();
        config.max_preview_bytes = 4;
        let snapshot = classify_file("big.txt", b"hello world", &config).unwrap();
        assert!(matches!(snapshot, FileSnapshot::TooLarge { .. }));
    }

    #[test]
    fn classify_ignored_for_builtin_deny() {
        let snapshot = classify_file(".env.local", b"SECRET=1", &cfg()).unwrap();
        match snapshot {
            FileSnapshot::Ignored { reason, .. } => {
                assert_eq!(reason, IgnoredReason::BuiltinCredential);
            }
            other => panic!("expected ignored, got {other:?}"),
        }
    }

    #[test]
    fn classify_ignored_for_user_rule() {
        let mut config = cfg();
        config.preflight.extend_with_ignore_text("*.bak\n");
        let snapshot = classify_file("file.bak", b"backup", &config).unwrap();
        match snapshot {
            FileSnapshot::Ignored { reason, .. } => {
                assert_eq!(reason, IgnoredReason::UserIgnore);
            }
            other => panic!("expected ignored, got {other:?}"),
        }
    }

    #[test]
    fn sha256_is_deterministic_lowercase_hex() {
        let h = sha256_hex(b"hello");
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn language_for_path_recognises_common_extensions() {
        assert_eq!(language_for_path("src/lib.rs"), Some("rust"));
        assert_eq!(language_for_path("page.tsx"), Some("typescript"));
        assert_eq!(language_for_path("README.md"), Some("markdown"));
        assert_eq!(language_for_path("Dockerfile"), None);
    }

    #[test]
    fn validate_oid_accepts_lowercase_hex() {
        validate_oid("abcdef0123456789abcdef0123456789abcdef01").unwrap();
        validate_oid("a1c2").unwrap();
        validate_oid("ABCDEF").expect_err("uppercase rejected");
        validate_oid("not-hex").expect_err("non-hex rejected");
        validate_oid("abc").expect_err("too short rejected");
        validate_oid(&"a".repeat(65)).expect_err("too long rejected");
    }

    #[test]
    fn validate_ref_name_accepts_full_refs() {
        validate_ref_name("refs/heads/main").unwrap();
        validate_ref_name("refs/tags/v1.0.0").unwrap();
        validate_ref_name("refs/heads/").expect_err("empty short rejected");
        validate_ref_name("refs/remotes/origin/main").expect_err("non-publishable rejected");
        validate_ref_name("main").expect_err("short-only rejected");
    }

    #[test]
    fn detect_ambiguous_short_refs_reports_collisions() {
        let refs = vec![
            RefInput {
                ref_name: "refs/heads/dev".to_string(),
                target_oid: "abcdef01".to_string(),
                revision_oid: "abcdef01".to_string(),
            },
            RefInput {
                ref_name: "refs/tags/dev".to_string(),
                target_oid: "deadbeef".to_string(),
                revision_oid: "deadbeef".to_string(),
            },
            RefInput {
                ref_name: "refs/heads/main".to_string(),
                target_oid: "11223344".to_string(),
                revision_oid: "11223344".to_string(),
            },
        ];
        let collisions = detect_ambiguous_short_refs(&refs);
        assert_eq!(collisions, vec!["dev".to_string()]);
    }
}
